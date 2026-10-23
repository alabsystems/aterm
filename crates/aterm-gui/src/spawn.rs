// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Session spawning, PTY reader lifecycle, shell integration setup, and child proxy recursion provisioning.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use aterm_core::terminal::{ClipboardAccess, ClipboardOperation, Terminal};
use aterm_session::sink::SinkWriter;
use aterm_session::{EdgeTable, EdgeToken, LaunchNonce, Op, SessionId};
use aterm_window::event_loop::EventLoopProxy;

use crate::{
    Session, SessionCtx, Wake, WindowId, control, control_auth, notify, proxy, session_store,
    term_lock,
};

/// The shell-integration loader's load-once guard variable, which the shipped zsh/bash scripts check and then export.
pub(crate) const SHELL_INTEGRATION_LOADED_GUARD: &str = "ATERM_SHELL_INTEGRATION_INSTALLED";

/// The runtime outcome of the most recent shell-integration preparation, as opposed to the advertised capability constant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ShellIntegrationOutcome {
    /// The loader was written and injected.
    Prepared,
    /// No integration script exists for this shell (named as selected/`$SHELL`).
    UnknownShell(String),
    /// The loader cache could not be written (the `std::io::Error` text).
    WriteFailed(String),
    /// Config `shell_args` ask for a start no integration can stand in for (a command, a script, POSIX mode, …).
    ArgsBypass(String),
}

static SHELL_INTEGRATION_OUTCOME: std::sync::Mutex<Option<ShellIntegrationOutcome>> =
    std::sync::Mutex::new(None);

/// The recorded runtime outcome, `None` before any integrated spawn was attempted (e.g. an `-e <cmd>`-only run, where no promise is broken because no interactive shell exists to type into).
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
    /// A windowed aterm's launch, once it holds the sessions it will run and before the first of them is spawned or adopted.
    Launch,
    /// A session spawn: `adopted` when it re-adopts a running shell (a seamless update's handoff, the keeper), `spare` when it is a warm spare nobody has asked for yet ([`crate::spare`]).
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
    /// This process runs from an app bundle that is not the release `aterm.app` (a dev bundle from `ship dev-app`, a renamed copy).
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
    // Unit tests construct `App`s that reach spawn seams, and headless
    // instances are tests and agents' private instances: neither may write
    // the `$HOME` it happened to inherit. A windowed aterm is where this runs.
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

/// The launch's first frame is on glass (a headless instance, which has none, asks at its first park).
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
    /// `SHELL` for the child when the broken shell came from `$SHELL`, which the child would otherwise inherit.
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
mod substitute_shell_tests {
    use super::{shell_to_start, substitute_shell};

    #[test]
    fn only_a_shell_that_cannot_start_is_replaced_and_the_pane_says_so() {
        assert_eq!(substitute_shell(Some("/bin/sh"), None), None);
        assert_eq!(substitute_shell(Some("sh"), None), None, "found on PATH");
        let sub = substitute_shell(Some("/nonexistent/aterm-test-zsh"), Some(&[]))
            .expect("a missing shell is replaced");
        assert!(
            aterm_pty::shell_cannot_start(
                std::ffi::OsStr::new(&sub.program),
                aterm_pty::ShellOrigin::Override
            )
            .is_none(),
            "the substitute can start: {sub:?}"
        );
        let said = "[aterm: `/nonexistent/aterm-test-zsh` does not exist (set by --shell or \
                    `shell` in aterm.toml); started ";
        assert!(
            sub.notice.starts_with(said)
                && sub.notice.contains(&format!("started {}, ", sub.program))
                && sub.notice.ends_with(" instead]"),
            "{sub:?}"
        );
        assert_eq!(sub.shell_env, None, "$SHELL was not what broke");
    }

    #[test]
    fn a_substitute_starts_without_the_broken_shells_args() {
        let args = ["--no-config".to_string()];
        let broken = Some("/nonexistent/aterm-test-fish");
        let sub = substitute_shell(broken, Some(&args)).expect("a missing shell is replaced");
        assert!(
            sub.notice.ends_with(" instead, without `shell_args`]"),
            "{sub:?}"
        );
        assert_eq!(
            shell_to_start(Some(&sub), broken, Some(&args)),
            (Some(sub.program.as_str()), None)
        );
        assert_eq!(
            shell_to_start(None, Some("/bin/sh"), Some(&args)),
            (Some("/bin/sh"), Some(&args[..]))
        );
    }
}

#[cfg(test)]
mod records_shell_start_tests {
    use super::records_shell_start;

    #[test]
    fn only_a_fresh_interactive_shell_records_a_start() {
        let false_cmd = vec![std::ffi::OsString::from("false")];
        assert!(records_shell_start(false, None), "a fresh shell");
        assert!(
            records_shell_start(false, Some(&[])),
            "an empty `-e` runs the shell, as at the spawn"
        );
        assert!(!records_shell_start(false, Some(&false_cmd)), "`-e false`");
        assert!(!records_shell_start(true, None), "an adopted shell");
        assert!(!records_shell_start(true, Some(&false_cmd)));
    }
}

/// Everything `spawn_session` needs to stand up a new tab's shell session, captured once at startup.
#[derive(Clone)]
pub(crate) struct SessionFactory {
    pub(crate) spawn_cap: aterm_cap::Cap<aterm_cap::effects::Spawn>,
    pub(crate) sandbox_cap: aterm_cap::Cap<aterm_sandbox::Sandbox>,
    /// Terminal-identity env (term/colorterm/lang/…) shared by every tab.
    pub(crate) env_add: Vec<(String, String)>,
    /// `-e <cmd>`: run this instead of `$SHELL` (also disables shell integration), as the OS's argv gave it ([`crate::cli::Cli::exec_command`]).
    pub(crate) exec_command: Option<Vec<std::ffi::OsString>>,
    /// Config `shell` / `--shell`.
    pub(crate) shell_override: Option<String>,
    /// Config `shell_args`: extra argv passed to `shell_override` (e.g. `-l -i`).
    pub(crate) shell_args: Option<Vec<String>>,
    /// `-d <dir>`: working directory for every tab's shell.
    pub(crate) cwd: Option<String>,
    /// OS-sandbox wrap (macOS Seatbelt sbpl).
    pub(crate) sandbox_wrap: Option<String>,
    /// Engine config (scrollback/cursor/theme/palette) applied to each tab's `Terminal`, byte-identical to the single-session path.
    pub(crate) terminal_config: Option<aterm_core::config::TerminalConfig>,
    /// Whether to inject OSC 133/633 shell integration, with a fresh nonce per tab.
    pub(crate) integrate: bool,
    /// Latency epoch shared across tabs.
    pub(crate) lat_epoch: Instant,
    /// The event loop's turn state ([`LoopIdle`]), stamped by the main thread and read by every tab's PTY reader ([`gated_output_wake`]).
    pub(crate) loop_idle: Arc<LoopIdle>,
    /// Desktop-notification channel shared by every tab (bounded, `notify::NOTIFY_QUEUE_CAP`).
    pub(crate) notify_tx: std::sync::mpsc::SyncSender<notify::NotifyMsg>,
    /// Config `allow_kitty_file_transfer` (default false).
    pub(crate) allow_kitty_file_transfer: bool,
    /// Config `temporal_recording` (default false).
    pub(crate) temporal_recording: bool,
    /// Overlap handoff (incoming side).
    pub(crate) defer_adopted_readers: bool,
    /// A tab spawned off the event loop (`tab_spawn`).
    pub(crate) reader_after_install: bool,
    /// The foreground-process-group probe each PTY gather samples for the foreground handback, and the status sweep reads for the group in front (`App::observe_session_statuses`).
    pub(crate) fg_probe: fn(i32) -> i32,
    /// Whether a foreground group is gone, the liveness half of the foreground handback (a stopped job, or `gdb -tui` handing the terminal to its inferior, still runs and keeps its modes).
    pub(crate) fg_gone: crate::foreground_handback::FgGone,
    /// Whether the shell `pid` runs a program it started at or after a wall-clock instant (unix epoch ms) where the foreground group cannot show it.
    pub(crate) program_probe: fn(i32, u64) -> bool,
    /// The kernel's name for the leader of foreground group `pgid` when no other process is in its group, read when a file citation asks whether a shell in front is the one a person types into (`path_link::runs_here`).
    pub(crate) lone_leader: fn(i32) -> Option<String>,
    /// Whether the program leading foreground group `pgid` has handed the terminal to another program in its group, asked when a bare Ctrl-r/Ctrl-s decides whether an agent's composer owns it (`App::front_agent_composer`).
    pub(crate) handed_terminal: fn(i32) -> bool,
    /// The image of foreground group `pgid`'s leader (card engine §3.4), read by each PTY gather at a foreground edge (given no last image) and by the parse stage's byte arm (given the group's last, which the read keeps for an unchanged process).
    pub(crate) edge_image: crate::holder_image::ReadImage,
    /// The edged inbox (card engine §3.5).
    pub(crate) edge_inbox: crate::holder_image::EdgeInbox,
    /// Whether each PTY reader runs the byte-driven arm (card engine §3.6).
    pub(crate) byte_arm: bool,
    /// Each PTY reader's process watch on its shell (card engine §3.3), made at attach.
    #[cfg(unix)]
    pub(crate) proc_watch: fn(i32) -> Option<aterm_uds::procwatch::ProcWatch>,
    /// Whether each PTY reader runs the dev `card-seen` probe ([`crate::holder_image::card_probe_requested`]).
    pub(crate) card_probe: bool,
    /// Overlap handoff (incoming side), armed like [`Self::defer_adopted_readers`].
    pub(crate) rekey_deferred: Option<Arc<crate::shell_rekey::Deferred>>,
    /// A warm spare's spawn (`crate::spare`), set only on the `aterm-spare` thread's factory copy, like [`Self::reader_after_install`].
    pub(crate) spare: Option<crate::spare::SpareSpawn>,
    /// The light/dark side of the theme this spawn's session will draw, where it draws one of its own (`App::spawn_shell_appearance`).
    #[cfg(windows)]
    pub(crate) shell_appearance: Option<aterm_types::Appearance>,
}

impl SessionFactory {
    /// What a spare spawned by this factory shares with a cold tab spawned by it now (`crate::spare`'s gate G4).
    pub(crate) fn spawn_fingerprint(&self) -> u64 {
        use std::hash::{Hash as _, Hasher as _};
        let Self {
            spawn_cap: _,
            sandbox_cap: _,
            env_add,
            exec_command,
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
        shell_override.hash(&mut h);
        shell_args.hash(&mut h);
        cwd.hash(&mut h);
        sandbox_wrap.hash(&mut h);
        integrate.hash(&mut h);
        temporal_recording.hash(&mut h);
        allow_kitty_file_transfer.hash(&mut h);
        aterm_containment::mode_or_containment()
            .level()
            .hash(&mut h);
        daily_driver_limits().open_files.hash(&mut h);
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

/// Whether `s` is a well-formed session id (`s-` + 20 lower-case hex chars / 80 bits), the shape [`SessionId::generate`] produces.
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
mod injected_identity_adoption_tests {
    use super::adopt_injected_identity_in;
    use crate::session_log::{Epochs, SessionLog};
    use aterm_session::{LaunchNonce, SessionId};
    use std::sync::Arc;

    #[test]
    fn two_launches_from_one_shell_do_not_share_an_identity() {
        let _guard = crate::identity_claim::rendezvous_test_guard();
        let dir = aterm_tempfile::tempdir().expect("scratch control dir");
        let premint = SessionId::generate();
        let nonce = LaunchNonce::generate();
        let (sid, hex) = (premint.as_str().to_string(), nonce.to_hex());

        let first =
            adopt_injected_identity_in(Some(dir.path()), Some(&sid), Some(&hex), &Epochs::new())
                .expect("the first launch adopts the premint");
        assert_eq!(first.0, premint);
        assert!(first.1.ct_eq(&nonce), "the nonce rides with the id");

        assert!(
            adopt_injected_identity_in(Some(dir.path()), Some(&sid), Some(&hex), &Epochs::new())
                .is_none(),
            "a second launch from the same shell must NOT answer to a live id"
        );

        crate::identity_claim::release_claims_for_test();
        let relaunch =
            adopt_injected_identity_in(Some(dir.path()), Some(&sid), Some(&hex), &Epochs::new())
                .expect("a relaunch after the first launch exits adopts the premint");
        assert_eq!(relaunch.0, premint);
        crate::identity_claim::release_claims_for_test();
    }

    #[test]
    fn adoption_is_refused_without_a_whole_identity_or_a_place_to_claim_it() {
        let dir = aterm_tempfile::tempdir().expect("scratch control dir");
        let sid = SessionId::generate().as_str().to_string();
        let hex = LaunchNonce::generate().to_hex();
        assert!(
            adopt_injected_identity_in(Some(dir.path()), Some(&sid), None, &Epochs::new())
                .is_none()
        );
        assert!(
            adopt_injected_identity_in(Some(dir.path()), None, Some(&hex), &Epochs::new())
                .is_none()
        );
        assert!(
            adopt_injected_identity_in(Some(dir.path()), Some("bad"), Some(&hex), &Epochs::new())
                .is_none()
        );
        assert!(
            adopt_injected_identity_in(None, Some(&sid), Some(&hex), &Epochs::new()).is_none(),
            "no control directory, no claim, no adoption"
        );
    }

    #[test]
    fn a_relaunch_under_the_premint_takes_an_epoch_above_its_predecessors() {
        let _guard = crate::identity_claim::rendezvous_test_guard();
        let dir = aterm_tempfile::tempdir().expect("scratch control dir");
        let premint = SessionId::generate();
        let (sid, hex) = (
            premint.as_str().to_string(),
            LaunchNonce::generate().to_hex(),
        );
        let hand = aterm_slog::Stamp::new(1_792_000_000_400, 400);
        let launch = || {
            let epochs = Arc::new(Epochs::new());
            let (id, _) =
                adopt_injected_identity_in(Some(dir.path()), Some(&sid), Some(&hex), &epochs)
                    .expect("the launch adopts the premint");
            let ep = SessionLog::starting(Arc::clone(&epochs), hand).log().ep();
            crate::identity_claim::record_epoch_in(dir.path(), &id, ep, true);
            crate::identity_claim::release_claims_for_test();
            ep
        };
        let first = launch();
        let second = launch();
        assert!(second > first, "the same second: {first} then {second}");
        assert_eq!(
            SessionLog::starting(Arc::new(Epochs::new()), hand)
                .log()
                .ep(),
            first,
            "NEGATIVE CONTROL: told nothing, the same second, the same epoch"
        );
        let recorded = || crate::identity_claim::recorded_epoch_in(dir.path(), &premint);
        assert_eq!(recorded(), Some(second));
        crate::identity_claim::record_epoch_in(dir.path(), &premint, second + 1, false);
        crate::identity_claim::record_epoch_in(dir.path(), &premint, first, false);
        assert_eq!(recorded(), Some(second + 1));
        let third = launch();
        assert!(third > second + 1, "{second} + 1 then {third}");
        let minted = SessionId::generate();
        crate::identity_claim::record_epoch_in(dir.path(), &minted, 7, false);
        assert_eq!(
            crate::identity_claim::recorded_epoch_in(dir.path(), &minted),
            None
        );
        let path = dir.path().join("claims").join(format!("{sid}.ep"));
        for foreign in [format!("{}\n", u32::MAX), "zz\n".to_string()] {
            std::fs::write(&path, foreign).expect("plant");
            assert_eq!(recorded(), None);
        }
        let fourth = launch();
        assert_eq!(recorded(), Some(fourth));

        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt as _;
            use std::os::unix::fs::symlink;
            let claims = dir.path().join("claims");
            let sentinel = dir.path().join("sentinel");
            std::fs::write(&sentinel, "71\n").expect("owned sentinel");
            let old_epoch_stage = claims.join(format!(".{sid}.ep.{}.tmp", std::process::id()));
            let old_marker_stage =
                claims.join(format!(".{sid}.successor.{}.tmp", std::process::id()));
            for stage in [&old_epoch_stage, &old_marker_stage] {
                symlink(&sentinel, stage).expect("owned stage alias");
            }
            crate::identity_claim::record_epoch_in(dir.path(), &premint, fourth + 1, true);
            crate::identity_claim::mark_successor_in(
                dir.path(),
                std::slice::from_ref(&premint),
                123,
            );
            assert_eq!(recorded(), Some(fourth + 1), "ordinary publication");
            assert_eq!(
                std::fs::read_to_string(claims.join(format!("{sid}.successor"))).unwrap(),
                "pid 123\n",
                "the marker publishes without consulting process liveness"
            );
            for stage in [&old_epoch_stage, &old_marker_stage] {
                assert!(std::fs::symlink_metadata(stage).unwrap().is_symlink());
                assert_eq!(std::fs::read_link(stage).unwrap(), sentinel);
            }
            assert_eq!(std::fs::read(&sentinel).unwrap(), b"71\n");

            std::fs::remove_file(&path).expect("remove our record");
            symlink(&sentinel, &path).expect("owned final alias");
            assert_eq!(recorded(), None, "a symlink is not a record");
            std::fs::remove_file(&path).expect("remove our alias");
            std::fs::hard_link(&sentinel, &path).expect("owned hardlink");
            assert_eq!(recorded(), None, "a multiply named inode is not a record");
            std::fs::remove_file(&path).expect("remove our hardlink");
            let fifo = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
            // SAFETY: a live NUL-terminated owned path; mkfifo retains nothing.
            assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
            assert_eq!(recorded(), None, "a FIFO with no writer is refused");
            std::fs::remove_file(&path).expect("remove our FIFO");
            std::fs::write(&path, "7               x").expect("over-bound record");
            assert_eq!(recorded(), None, "a capped prefix is not a whole record");
            crate::identity_claim::record_epoch_in(dir.path(), &premint, fourth + 2, true);
            assert_eq!(recorded(), Some(fourth + 2), "ordinary record reads again");
            assert_eq!(std::fs::read(&sentinel).unwrap(), b"71\n");
        }
    }
}

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
    use aterm_types::domain::ENV_PARENT_SESSION_ID;
    vec![
        (
            ENV_PARENT_SESSION_ID.to_string(),
            parent_sid.as_str().to_string(),
        ),
        (
            aterm_types::domain::ENV_MUX_BASE.to_string(),
            mux_env_signature_now(),
        ),
    ]
}

fn mux_env_signature_now() -> String {
    let get = |k: &str| std::env::var(k).unwrap_or_default();
    format!("{}|{}", get("TMUX"), get("STY"))
}

/// The controller-spawn observation hint.
pub(crate) fn provision_observe_env(origin_sid: &SessionId) -> Vec<(String, String)> {
    use aterm_types::domain::ENV_OBSERVE_SESSION_ID;
    vec![(
        ENV_OBSERVE_SESSION_ID.to_string(),
        origin_sid.as_str().to_string(),
    )]
}

/// Mint a fresh child identity and the three per-op capability edges (read/write/signal) this session grants over the child it is about to host.
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

/// Insert the parent-preminted edges into a child-side [`EdgeTable`] from the injected env values, binding each to the child's `self_id` (dst) and `nonce`.
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

    /// Adopt the master: move it into its session byte sink ([`master_sink`]), re-arming `FD_CLOEXEC` first (the outgoing process cleared it to survive the exec; left clear it would leak into every later subprocess).
    pub(crate) fn into_sink(self) -> Arc<SinkWriter> {
        #[cfg(unix)]
        let _ = aterm_pty::set_cloexec(self.0, true);
        let master = std::mem::ManuallyDrop::new(self).0;
        // SAFETY: `master` is the handed-off descriptor this handle owned solely
        // (`HandedMaster::new` takes ownership, and nothing else closes it); the
        // `ManuallyDrop` above gave that ownership up to the sink.
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
/// The caller owns `master` outright — no other owner will close it — and gives
/// that ownership up by this call. On Unix it is an open descriptor; on Windows
/// it is the opaque ConPTY registry key, which `OwnedMaster::adopt` takes over
/// under the same contract.
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
    /// The outgoing session's pool id, so the incoming boot places this shell back into its original pane.
    pub local_id: u64,
    /// The PTY master fd inherited across the exec (cloexec was cleared before it), owned until [`spawn_session`] moves it into the session's sink.
    pub master: HandedMaster,
    /// The (still-running) child shell's pid == pgid.
    pub pid: i32,
    /// The session's restored stable fabric identity (from the handoff manifest).
    pub sid: SessionId,
    /// The session's restored launch nonce.
    pub nonce: LaunchNonce,
    /// Screen carry: the outgoing engine's checkpoint, hydrated before the reader starts so the window shows the pre-update screen.
    pub checkpoint: Option<aterm_core::terminal::TerminalCheckpoint>,
    /// Control carry (`crate::handoff_carry`).
    pub control: Option<crate::handoff_carry::ControlCarry>,
    /// `checkpoint` is not the program's last screen.
    pub repaint: bool,
    /// This shell was spawned by a build before self-healing sessions (`session_store::predates_path_self_heal`).
    pub frozen_path: bool,
    /// Broadcast opt-ins: the record's `topic add` set as `"<topic> <since>"` rows, already validated, re-seeded onto the adopted `SessionFabric` so a seamless update does not unsubscribe an agent from its topics.
    pub topics: Vec<String>,
    /// The process group the outgoing reader last saw holding the terminal (`SessionRecord::fg_holder`).
    pub fg_holder: i32,
    /// The record says this shell reads a re-key file at every prompt (`shell_rekey`).
    pub rekey: bool,
    /// The record says this shell runs an integration loader that reads a body pointer at every prompt (`shell_body`).
    pub loader: bool,
    /// History carry (`crate::handoff_history`).
    pub history: crate::handoff_history::AdoptedHistory,
    /// The handoff record's `"<origin> <reason>"` hold rows, unparsed, one per standing origin (`SessionRecord::hold_rows`, or a keeper meta's).
    pub holds: Vec<String>,
    /// The fallback title for a checkpoint that carried no title of its own.
    pub title: String,
    /// Another supervisor's lease, the record's `"<holder> <remaining_ms>"` row, unparsed.
    pub supervisor: Option<String>,
    /// The record's keyed escalations (`"<owner> <pct-text>"` attention rows), unparsed, seeded beside the lease.
    pub attention_owners: Vec<String>,
    /// Whether this build's in-GUI host holds off the session for its adopted-claim grace (`SessionRecord::claim_grace`).
    pub claim_grace: bool,
    /// The build that handed this shell across (the manifest's `outgoing_build`; `None` from a producer that wrote none), named by the handoff gap each of the session's recorders opens with ([`mark_handoff_gap`]).
    pub outgoing_build: Option<u64>,
    /// The PTY keeper's copy.
    pub keeper: Option<crate::keeper_link::Claim>,
    /// The render debt: what the outgoing engine owed the alternate screen this session restores onto (rows moved, drawn columns cut) (`SessionRecord::render_debt`), only when non-zero and the screen was carried.
    pub render_debt: Option<aterm_core::terminal::RenderDebt>,
    /// The mailbox the handoff carried.
    pub fabric: crate::fabric::FabricCarry,
    /// The record's newest timeline id.
    pub timeline_id: u64,
    /// The session log's epoch, node-id counters and seq count the record carried (`SessionRecord::log_ids`, `SessionRecord::log_seq`).
    pub log: crate::session_log::LogAdopt,
    /// The pair the crash journal image this window takes over at its Commit keys this shell by, when the outgoing process held an image that was not its own (`SessionRecord::journal_key`).
    pub journal_key: Option<(u32, u64)>,
    /// The record's task row (`SessionRecord::task`), unparsed.
    pub task: Option<String>,
    /// The record's Autopilot end row (`SessionRecord::autopilot_end`), unparsed.
    pub autopilot_end: Option<String>,
}

/// A seamless update's carried end onto an adopted session's timeline, before the session is registered (`docs/DESIGN-harness-autopilot- 2026-10-15.md` §11.2.3, item 8).
pub(crate) fn seed_adopted_end(
    timeline: &std::sync::Mutex<crate::session_timeline::SessionTimeline>,
    row: Option<&str>,
) {
    let Some(told) = row
        .and_then(crate::harness_end_carry::EndCarry::parse)
        .and_then(|carry| carry.end)
        .map(crate::harness_end_carry::CarriedEnd::told)
    else {
        return;
    };
    timeline
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .seed_task_face_held(told.0, told.1);
}

/// Mark an adopted session's timeline, cast and temporal recorder as starting at the handoff ([`crate::session_timeline::HandoffGap`]), since none of them crosses the update.
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
        tl.record("handoff", gap.payload());
        let at = tl.slog().now();
        tl.slog_mut().adopt(log, keeper, from_build, at);
    }
    cast.lock()
        .unwrap_or_else(|p| p.into_inner())
        .mark_handoff(gap);
    temporal
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .mark_handoff(gap);
}

/// Restore the broadcast opt-ins and standing halt a seamless update carried onto the adopting session's `SessionFabric`.
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

/// Authorize the adopted shell's nonce on the successor engine after `restore_checkpoint` restored `require_shell_integration_nonce`.
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
            // Adopted with no screen to restore: the shell's mark authority
            // still crosses (see `hold_adopted_shell_nonce_requirement`).
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

/// Install an adopted session's carried archive ([`hydrate_adopted_engine`]), and say so when it carried rows that were refused (`AltArchive::import`: an archive switched off here, or indices no archive reaches), so a fresh `offscreen` after the update is named in the log.
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

/// The temporal recorder's first keyframe of session `id`'s engine, taken before its reader runs.
pub(crate) fn seed_temporal_keyframe(
    term: &Mutex<Terminal>,
    temporal: &std::sync::Mutex<crate::temporal::TemporalRecorder>,
    id: u64,
) -> bool {
    let (checkpoint, at) = {
        let engine = term_lock(term);
        match engine.checkpoint_continuing() {
            Some(checkpoint) => (checkpoint, crate::temporal::SpinePre::take(&engine)),
            None => {
                aterm_log::warn!(
                    "temporal: session {id} starts inside a {} no keyframe can carry; its \
                     recording is not seeded",
                    engine
                        .partial_sequence_state()
                        .unwrap_or("partial sequence")
                );
                return false;
            }
        }
    };
    let mut rec = temporal.lock().unwrap_or_else(|p| p.into_inner());
    rec.record_keyframe(checkpoint);
    rec.spine_watermark().seed(at);
    true
}

/// Install the fallback title (`record_title`: [`Adopted::title`], the park's layout title or the handoff record's copy) when the checkpoint carried none, via `Terminal::restore_title` (capped, controls stripped, title epoch bumped).
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

/// The console-host keyboard posture of the PTY behind `master` ([`Terminal::set_console_host_posture`]), keyed on the resize seam policy the session resizes under ([`crate::app_render::pty_resize_policy`]), so a `cast drift` replay under that policy rebuilds the same engine.
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
        // Adopted: the shell is already running with its original env (including
        // the nonce minted at its first spawn), so nothing is injected.
        (Vec::new(), None, None)
    } else if factory.integrate {
        match prepare_shell_integration(shell_override, shell_args) {
            Some((si_env, argv_override, nonce, shell)) => {
                integrated_shell = Some(shell);
                let mut env = factory.env_add.clone();
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
        // Adopted: the child keeps its first-spawn recursion tokens; env cannot be
        // re-injected, so nothing is re-provisioned.
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
                if spare.is_some() && start.fell_back.is_some() {
                    return Err(std::io::Error::other(crate::spare::FolderFallsBack));
                }
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
                // SAFETY: `master` is this session's forkpty master, freshly returned
                // and owned solely here; the sink takes it over.
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
    let temporal = Arc::new(std::sync::Mutex::new(
        crate::temporal::TemporalRecorder::new(),
    ));
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
        meta: std::sync::Mutex::new(crate::session_timeline::SessionMeta::default()),
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
        update_parked: Default::default(),
        watchers: observed.watchers,
        frame: Default::default(),
        fenced_writes: Default::default(),
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
    crate::task_record::seed_carried(&ctx, adopt_task.as_deref());
    seed_adopted_end(&ctx.timeline, adopt_end.as_deref());

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
        if adopted {
            fit_adopted_log(&term, &ctx.timeline);
        }
    }
    #[cfg(unix)]
    if adopt_checkpoint.is_some() {
        adopt_restored_holder(
            &term,
            &ctx.timeline,
            master,
            pid,
            factory.fg_probe,
            factory.edge_image,
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

    if factory.temporal_recording {
        seed_temporal_keyframe(&term, &temporal, id);
    }

    if let Some(nonce) = shell_nonce {
        let mut t = term_lock(&term);
        if !lost_nonce {
            t.authorize_shell_integration(nonce);
        }
        t.set_require_shell_integration_nonce(true);
    }

    let reach = session_host_reach();
    match spare {
        Some(s) => crate::spare::wire_spare_taint(&term, &s.link),
        None => arm_host_reach(&term, reach, factory, proxy, id, window, &ctx.witness),
    }

    term_lock(&term).apply_policy_engine(aterm_policy::engine::PolicyEngine::new(
        aterm_policy::profiles::standard(),
    ));

    configure_palette_refusal(&term);

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
        temporal_writer_join: None,
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
            // No thread: end the pulse now rather than leave the program at
            // `rows-1`. It may not repaint, but it is never left mis-sized.
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

/// Seed the restored engine with the render debt its predecessor carried (`Adopted::render_debt`) and let the resize ledger look at it once, at attach.
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

/// Whether an adopted session's PTY gets the rows-1 → rows size pulse that makes its program redraw.
#[cfg(any(unix, test))]
pub(crate) fn adoption_needs_size_pulse(
    checkpoint: Option<&aterm_core::terminal::TerminalCheckpoint>,
    repaint: bool,
) -> bool {
    repaint || checkpoint.is_some_and(|cp| cp.modes.alternate_screen)
}

#[cfg(test)]
mod adoption_wiring_tests {

    #[test]
    fn an_adopted_timeline_opens_with_a_handoff_gap() {
        use std::sync::Mutex as StdMutex;
        let fresh_cast = crate::cast::CastRecorder::new(80, 24).to_asciicast();
        let fresh_temporal = crate::temporal::TemporalRecorder::new().status_tail();
        assert!(
            !crate::session_timeline::SessionTimeline::default().muse_cards_need_account(),
            "a fresh terminal can bootstrap its first anonymous usage card"
        );
        for (from_build, word, json) in [
            (Some(812_u64), "812", "\"from_build\": 812"),
            (None, "-", "\"from_build\": null"),
        ] {
            let timeline = StdMutex::new(crate::session_timeline::SessionTimeline::default());
            let cast = StdMutex::new(crate::cast::CastRecorder::new(80, 24));
            let temporal = StdMutex::new(crate::temporal::TemporalRecorder::new());
            super::mark_handoff_gap(
                &timeline,
                &cast,
                &temporal,
                from_build,
                0,
                Default::default(),
                false,
            );
            assert!(
                timeline.lock().unwrap().muse_cards_need_account(),
                "a successor cannot attribute the predecessor's anonymous cards"
            );
            timeline
                .lock()
                .unwrap()
                .record("spawned", "state=spawning".to_string());
            let rows: Vec<(u64, &str, String)> = timeline
                .lock()
                .unwrap()
                .since(None)
                .map(|e| (e.id, e.kind, e.payload.to_string()))
                .collect();
            assert_eq!(
                rows[0],
                (1, "handoff", format!("carried=0 from_build={word}")),
                "the handoff row opens the adopted timeline: {rows:?}"
            );
            let header = cast.lock().unwrap().to_asciicast();
            let header = header.lines().next().unwrap().to_string();
            assert!(
                header.contains("\"aterm_handoff\": {\"carried\": 0, ") && header.contains(json),
                "the cast header discloses the handoff: {header}"
            );
            assert_eq!(
                temporal.lock().unwrap().status_tail(),
                format!(" carried=0 from_build={word}"),
                "`temporal status` reports the handoff gap"
            );
        }
        assert!(!fresh_cast.contains("aterm_handoff"), "{fresh_cast}");
        assert_eq!(fresh_temporal, "", "a fresh recorder's status is unchanged");
    }

    #[test]
    fn a_keeper_recovery_shows_the_crashed_windows_anchor_its_first_row() {
        use crate::session_timeline::{HANDOFF_ID_RESERVE, SessionTimeline};
        use std::sync::Mutex as StdMutex;
        let adopt = |timeline_id: u64, keeper: bool| {
            let timeline = StdMutex::new(SessionTimeline::default());
            super::mark_handoff_gap(
                &timeline,
                &StdMutex::new(crate::cast::CastRecorder::new(80, 24)),
                &StdMutex::new(crate::temporal::TemporalRecorder::new()),
                (!keeper).then_some(812),
                timeline_id,
                Default::default(),
                keeper,
            );
            timeline
        };
        let mut producer = SessionTimeline::default();
        for _ in 0..3 {
            producer.record("state-change", "state=alive".to_string());
        }
        let updated = adopt(producer.handoff_id(), false);
        updated
            .lock()
            .unwrap()
            .record("state-change", "state=alive".to_string());
        let anchor = updated.lock().unwrap().high_id().expect("rows");
        assert_eq!(anchor, 3 + HANDOFF_ID_RESERVE + 2);

        let recovered = adopt(0, true);
        let page = |anchor: u64| -> Vec<(u64, &'static str)> {
            recovered
                .lock()
                .unwrap()
                .since(Some(anchor))
                .map(|e| (e.id, e.kind))
                .collect()
        };
        assert_eq!(
            page(anchor),
            vec![(1, "handoff")],
            "the crashed window's anchor reads the recovery's first row at once"
        );
        recovered
            .lock()
            .unwrap()
            .record("state-change", "state=alive".to_string());
        assert_eq!(
            page(anchor),
            vec![(1, "handoff"), (2, "state-change")],
            "and every row after it"
        );
        assert_eq!(page(1), vec![(2, "state-change")], "negative control");
        assert_eq!(page(2), vec![], "negative control: at the newest, nothing");
    }
}

#[cfg(test)]
mod temporal_seed_tests {
    use super::seed_temporal_keyframe;
    use crate::temporal::TemporalRecorder;
    use aterm_core::terminal::Terminal;
    use std::sync::Mutex;

    #[test]
    fn an_engine_adopted_mid_sequence_seeds_a_keyframe_that_continues_it() {
        let term = Mutex::new(Terminal::new(4, 20));
        crate::term_lock(&term).process(b"$ \x1b[38;5;19");
        assert!(!crate::term_lock(&term).parser_is_ground(), "PRECONDITION");
        let recorder = Mutex::new(TemporalRecorder::new());
        assert!(seed_temporal_keyframe(&term, &recorder, 1), "seeded");
        assert_eq!(recorder.lock().unwrap().keyframe_count(), 1);

        crate::term_lock(&term).process(b"6mX");
        recorder.lock().unwrap().record_raw_in(b"6mX");
        let replay = recorder
            .lock()
            .unwrap()
            .replay_at(None)
            .expect("the keyframe replays");
        let live = crate::term_lock(&term).row_text(0);
        assert_eq!(
            live.as_deref().map(str::trim_end),
            Some("$ X"),
            "PRECONDITION: the live engine finished the sequence"
        );
        assert_eq!(
            replay.row_text(0),
            live,
            "the replay continues the sequence instead of printing its tail"
        );

        let hooked = Mutex::new(Terminal::new(4, 20));
        crate::term_lock(&hooked).process(b"\x1bP$qm");
        let unseeded = Mutex::new(TemporalRecorder::new());
        assert!(!seed_temporal_keyframe(&hooked, &unseeded, 2));
        assert_eq!(unseeded.lock().unwrap().keyframe_count(), 0);
    }
}

#[cfg(test)]
mod pty_keyboard_posture_tests {
    use super::{apply_pty_keyboard_posture, new_live_terminal};
    use crate::term_lock;
    use aterm_core::terminal::Terminal;
    use aterm_types::keyboard::{
        Key, KeyEventType, KeyboardMode, Modifiers, NamedKey, encode_key_with_layout,
    };
    use std::sync::Mutex;

    const KITTY_BITS: KeyboardMode = KeyboardMode::DISAMBIGUATE_ESC_CODES
        .union(KeyboardMode::REPORT_EVENT_TYPES)
        .union(KeyboardMode::REPORT_ALTERNATE_KEYS)
        .union(KeyboardMode::REPORT_ALL_KEYS_AS_ESC)
        .union(KeyboardMode::REPORT_ASSOCIATED_TEXT);

    fn live_engine() -> Mutex<Terminal> {
        Mutex::new(new_live_terminal(24, 80, None, None))
    }

    fn probe_then_push(term: &Mutex<Terminal>) -> (Option<Vec<u8>>, KeyboardMode, [Vec<u8>; 2]) {
        let mut t = term_lock(term);
        t.process(b"\x1b[?u");
        let reply = t.take_response();
        t.process(b"\x1b[>1u");
        let mode = t.keyboard_mode();
        let press = |ch, mods| {
            encode_key_with_layout(&Key::Character(ch), mods, mode, KeyEventType::Press, None)
        };
        (
            reply,
            mode.intersection(KITTY_BITS),
            [press('l', Modifiers::CTRL), press('a', Modifiers::ALT)],
        )
    }

    #[test]
    fn a_native_session_keeps_the_kitty_keyboard_protocol() {
        for master in [-1, 0x7fff_ffff] {
            let term = live_engine();
            apply_pty_keyboard_posture(&term, master);
            let (reply, kitty, keys) = probe_then_push(&term);
            assert_eq!(reply.as_deref(), Some(&b"\x1b[?0u"[..]), "master {master}");
            assert_eq!(
                kitty,
                KeyboardMode::DISAMBIGUATE_ESC_CODES,
                "master {master}"
            );
            assert_eq!(keys, [b"\x1b[108;5u".to_vec(), b"\x1b[97;3u".to_vec()]);
        }
    }

    #[test]
    fn a_native_session_ignores_a_programs_win32_input_request() {
        for master in [-1, 0x7fff_ffff] {
            let term = live_engine();
            apply_pty_keyboard_posture(&term, master);
            let mut t = term_lock(&term);
            t.process(b"\x1b[?9001h");
            let mode = t.mode_mirror().keyboard_mode();
            assert!(!mode.contains(KeyboardMode::WIN32_INPUT), "master {master}");
            let press = |key: Key, mods| {
                encode_key_with_layout(&key, mods, mode, KeyEventType::Press, None)
            };
            assert_eq!(
                press(Key::Named(NamedKey::Escape), Modifiers::empty()),
                b"\x1b",
                "master {master}"
            );
            assert_eq!(
                press(Key::Named(NamedKey::Enter), Modifiers::SHIFT),
                b"\n",
                "master {master}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn a_conpty_session_does_not_advertise_the_kitty_keyboard_protocol() {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn CreateEventW(
                attrs: *mut std::ffi::c_void,
                manual_reset: i32,
                initial_state: i32,
                name: *const u16,
            ) -> isize;
        }
        // SAFETY: NULL attrs and name; manual-reset, initially unsignaled.
        // `adopt_handoff` owns the handle from its `Ok` on.
        let client = unsafe { CreateEventW(std::ptr::null_mut(), 1, 0, std::ptr::null()) };
        assert_ne!(client, 0, "CreateEventW");
        let master = aterm_pty::adopt_handoff(0, 0, 0, client)
            .expect("register a ConPTY session")
            .master;
        let term = live_engine();
        apply_pty_keyboard_posture(&term, master);
        aterm_pty::close_master(master);

        let (reply, kitty, keys) = probe_then_push(&term);
        assert_eq!(reply, None, "a ConPTY session must not answer CSI ? u");
        assert!(
            kitty.is_empty(),
            "a push must not reach the encoder: {kitty:?}"
        );
        assert_eq!(keys, [vec![0x0c], b"\x1ba".to_vec()]);
        term_lock(&term).process(b"\x1b[?9001h");
        assert!(
            term_lock(&term)
                .keyboard_mode()
                .contains(KeyboardMode::WIN32_INPUT)
        );
    }

    #[test]
    fn an_openconsole_session_does_not_advertise_the_kitty_keyboard_protocol() {
        let term = live_engine();
        crate::alt_screen_resync::apply_posture(
            &mut term_lock(&term),
            aterm_core::grid::ResizePolicy::ConPty,
            Some(aterm_pty::ConsoleHostKind::OpenConsole),
        );
        let (reply, kitty, keys) = probe_then_push(&term);
        assert_eq!(
            reply, None,
            "an OpenConsole session must not answer CSI ? u"
        );
        assert!(
            kitty.is_empty(),
            "a push must not reach the encoder: {kitty:?}"
        );
        assert_eq!(keys, [vec![0x0c], b"\x1ba".to_vec()]);
    }
}

#[cfg(test)]
mod adoption_pulse_tests {
    use super::adoption_needs_size_pulse;

    #[test]
    fn a_repaint_adoption_pulses_on_either_screen_and_an_exact_shell_does_not() {
        let main = aterm_core::terminal::Terminal::new(24, 80)
            .checkpoint_carry(0)
            .expect("a fresh engine is Ground");
        let alt = {
            let mut terminal = aterm_core::terminal::Terminal::new(24, 80);
            terminal.process(b"\x1b[?1049h");
            terminal
                .checkpoint_carry(0)
                .expect("Ground after the DECSET")
        };
        assert!(
            !adoption_needs_size_pulse(Some(&main), false),
            "an exact main-screen shell keeps its screen"
        );
        assert!(
            adoption_needs_size_pulse(Some(&alt), false),
            "an alt-screen TUI is told to redraw, as before"
        );
        assert!(
            adoption_needs_size_pulse(Some(&main), true),
            "a blank main screen is repainted"
        );
        assert!(
            adoption_needs_size_pulse(None, true),
            "and so is a session adopted onto a blank engine with no checkpoint at all"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_adoption_pulse_ends_on_the_engines_grid_not_the_windows() {
        let (_master_fd, _slave_fd, master, slave) = open_pty_pair();
        let pane = {
            let mut terminal = aterm_core::terminal::Terminal::new(24, 39);
            terminal.process(b"\x1b[?1049hvim");
            terminal.checkpoint_carry(0).expect("Ground")
        };
        let term = std::sync::Arc::new(std::sync::Mutex::new(aterm_core::terminal::Terminal::new(
            24, 80,
        )));
        term.lock().expect("engine").restore_checkpoint(&pane);
        aterm_pty::resize_with_cell_px(master, 24, 80, Some((8, 16)));
        let holding = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        super::pulse_adopted_pty(
            master,
            &term,
            &holding,
            Some((8, 16)),
            super::PulseHold::BackToBack,
        );
        assert_eq!(
            slave_winsize(slave),
            (24, 39, 39 * 8, 24 * 16),
            "an exact restore's pulse is back to back, at the engine's grid"
        );
        assert!(!holding.load(std::sync::atomic::Ordering::Acquire));
        aterm_pty::resize_with_cell_px(master, 24, 80, Some((8, 16)));
        let pulsed = std::time::Instant::now();
        super::pulse_adopted_pty(
            master,
            &term,
            &holding,
            Some((8, 16)),
            super::PulseHold::Held,
        );
        let deadline = pulsed + std::time::Duration::from_secs(60);
        let ended = loop {
            let ws = slave_winsize(slave);
            if ws.0 == 24 || std::time::Instant::now() > deadline {
                break ws;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        drop(term.lock().expect("engine"));
        assert!(
            !holding.load(std::sync::atomic::Ordering::Acquire),
            "the hold is lowered when the pulse ends"
        );
        assert_eq!(
            ended,
            (24, 39, 39 * 8, 24 * 16),
            "the program sees the pane it is drawn in"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_adoption_pulse_holds_the_transient_size_for_the_program_to_read() {
        let (_master_fd, _slave_fd, master, slave) = open_pty_pair();
        let alt = {
            let mut terminal = aterm_core::terminal::Terminal::new(30, 100);
            terminal.process(b"\x1b[?1049hclaude");
            terminal.checkpoint_carry(0).expect("Ground")
        };
        let term = std::sync::Arc::new(std::sync::Mutex::new(aterm_core::terminal::Terminal::new(
            30, 100,
        )));
        term.lock().expect("engine").restore_checkpoint(&alt);
        aterm_pty::resize_with_cell_px(master, 30, 100, Some((8, 16)));
        let cached = slave_winsize(slave);
        let holding = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

        let mut held = None;
        super::pulse_adopted_pty_with(master, &term, &holding, Some((8, 16)), |restore| {
            held = Some(restore);
        });
        assert!(
            holding.load(std::sync::atomic::Ordering::Acquire),
            "the session's hold stands between the halves"
        );
        let read = slave_winsize(slave);
        assert_ne!(
            (read.0, read.1),
            (cached.0, cached.1),
            "the program must read a CHANGED size or it never repaints"
        );
        assert_eq!((read.0, read.1), (29, 100), "the transient is rows-1");
        {
            let engine = term.lock().expect("engine");
            assert_eq!(
                (engine.rows(), engine.cols()),
                (30, 100),
                "only the PTY carries the transient; the engine keeps its grid"
            );
        }
        held.take()
            .expect("the second half is handed to the hold")
            .run();
        assert_eq!(
            slave_winsize(slave),
            (30, 100, 800, 480),
            "the pulse ends on the engine's grid"
        );
        assert!(
            !holding.load(std::sync::atomic::Ordering::Acquire),
            "and lowers the hold"
        );

        let mut held = None;
        super::pulse_adopted_pty_with(master, &term, &holding, Some((8, 16)), |restore| {
            held = Some(restore);
        });
        term.lock().expect("engine").resize(40, 120);
        aterm_pty::resize_with_cell_px(master, 40, 120, Some((8, 16)));
        held.take().expect("held").run();
        assert_eq!(
            (slave_winsize(slave).0, slave_winsize(slave).1),
            (40, 120),
            "the second half writes the engine's grid as it is then"
        );

        super::pulse_adopted_pty_with(master, &term, &holding, Some((8, 16)), drop);
        assert_eq!((slave_winsize(slave).0, slave_winsize(slave).1), (40, 120));
        assert!(!holding.load(std::sync::atomic::Ordering::Acquire));
    }

    #[cfg(unix)]
    fn open_pty_pair() -> (std::os::fd::OwnedFd, std::os::fd::OwnedFd, i32, i32) {
        use std::os::fd::FromRawFd;
        let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
        // SAFETY: both fds were just opened above and are owned here.
        let (m, s) = unsafe {
            (
                std::os::fd::OwnedFd::from_raw_fd(master),
                std::os::fd::OwnedFd::from_raw_fd(slave),
            )
        };
        (m, s, master, slave)
    }

    #[cfg(unix)]
    fn slave_winsize(slave: i32) -> (u16, u16, u16, u16) {
        let mut ws = libc::winsize {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: `slave` is open; `ws` is a valid out-param for TIOCGWINSZ.
        assert_eq!(unsafe { libc::ioctl(slave, libc::TIOCGWINSZ, &mut ws) }, 0);
        (ws.ws_row, ws.ws_col, ws.ws_xpixel, ws.ws_ypixel)
    }
}

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

    /// Point-of-no-failure release, after the parent's Commit or once a tab spawned off the loop is registered (`tab_spawn`).
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
mod shell_integration_warn_tests {
    use super::{ShellIntegrationOutcome, first_sighting};

    #[test]
    fn a_failed_outcome_is_new_once_per_process() {
        let seen = std::sync::Mutex::new(Vec::new());
        let sh = ShellIntegrationOutcome::UnknownShell("/bin/sh".to_string());
        assert!(first_sighting(&seen, &sh), "the first /bin/sh tab");
        for _ in 0..20 {
            assert!(!first_sighting(&seen, &sh), "every later /bin/sh tab");
        }
        assert!(first_sighting(
            &seen,
            &ShellIntegrationOutcome::UnknownShell("nu".to_string())
        ));
        let full = ShellIntegrationOutcome::WriteFailed("disk full".to_string());
        assert!(first_sighting(&seen, &full));
        assert!(!first_sighting(&seen, &full));
        assert!(first_sighting(
            &seen,
            &ShellIntegrationOutcome::WriteFailed("read-only".to_string())
        ));
    }
}

#[cfg(test)]
mod agent_prime_tests {
    use super::{
        AgentPrimeOutcome, PrimeCue, PrimeSchedule, agent_prime_pass, agent_prime_permitted,
        run_agent_prime, take_pass,
    };
    use std::time::{Duration, Instant};

    #[test]
    fn an_uncommitted_candidate_writes_no_agents_file() {
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let home = scratch.path();
        std::fs::create_dir_all(home.join(".codex")).expect("detect codex");
        let prime = || {
            crate::test_env::scoped_unset("XDG_CONFIG_HOME", || {
                run_agent_prime(true, false, Some(home.to_path_buf()))
            })
        };
        assert_eq!(agent_prime_pass(|| false, prime), None);
        assert!(!home.join(".codex/AGENTS.md").exists());
        assert!(matches!(
            agent_prime_pass(|| true, prime),
            Some(AgentPrimeOutcome::Ran(_))
        ));
        assert!(home.join(".codex/AGENTS.md").is_file());
    }

    fn passes(cues: &[(PrimeCue, u64)]) -> Vec<usize> {
        let t0 = Instant::now();
        let mut last = None;
        let mut started = Vec::new();
        for (i, &(cue, s)) in cues.iter().enumerate() {
            if take_pass(cue, &mut last, t0 + Duration::from_secs(s)) {
                started.push(i);
            }
        }
        started
    }

    #[test]
    fn the_launch_primes_the_tabs_an_update_hands_over() {
        let adopted = PrimeCue::Spawn {
            adopted: true,
            spare: false,
        };
        let fresh = PrimeCue::Spawn {
            adopted: false,
            spare: false,
        };
        let spare = PrimeCue::Spawn {
            adopted: false,
            spare: true,
        };
        let successor = [
            (PrimeCue::Launch, 0),
            (adopted, 0),
            (adopted, 1),
            (spare, 2),
        ];
        assert_eq!(passes(&successor), [0], "the launch's pass, and only it");
        assert_eq!(
            passes(&successor[1..]),
            [0_usize; 0],
            "adoptions alone prime nothing"
        );
        assert_eq!(passes(&[(PrimeCue::Launch, 0), (fresh, 0)]), [0]);
        let later = [
            (PrimeCue::Launch, 0),
            (spare, 61),
            (PrimeCue::SpareAdopted, 62),
            (adopted, 200),
            (fresh, 200),
        ];
        assert_eq!(passes(&later), [0, 2, 4]);
    }

    fn started(steps: &[Option<(PrimeCue, u64)>], held: bool) -> Vec<(usize, PrimeCue)> {
        let t0 = Instant::now();
        let mut schedule = PrimeSchedule::AT_LAUNCH;
        if !held {
            assert_eq!(schedule.release(), None);
        }
        let mut out = Vec::new();
        for (i, step) in steps.iter().enumerate() {
            let start = match *step {
                Some((cue, s)) => schedule.admit(cue, t0 + Duration::from_secs(s)),
                None => schedule.release(),
            };
            out.extend(start.map(|cue| (i, cue)));
        }
        out
    }

    #[test]
    fn no_pass_starts_before_the_launchs_first_frame() {
        let fresh = PrimeCue::Spawn {
            adopted: false,
            spare: false,
        };
        let cold = [
            Some((PrimeCue::Launch, 0)),
            Some((fresh, 0)),
            None,
            None,
            Some((PrimeCue::SpareAdopted, 61)),
        ];
        assert_eq!(
            started(&cold, true),
            [(2, PrimeCue::Launch), (4, PrimeCue::SpareAdopted)]
        );
        assert_eq!(
            started(
                &[Some((PrimeCue::Launch, 0)), Some((fresh, 61)), None],
                true
            ),
            [(2, PrimeCue::Launch)]
        );
        assert_eq!(started(&[Some((fresh, 0)), None], true), [(1, fresh)]);
        assert_eq!(started(&[None, Some((fresh, 0))], true), [(1, fresh)]);
        assert_eq!(started(&[None, None], true), []);
        assert_eq!(
            started(&cold, false),
            [(0, PrimeCue::Launch), (4, PrimeCue::SpareAdopted)],
            "the control: unheld, the launch's pass starts at its cue"
        );
    }

    #[test]
    fn a_headless_instance_never_primes() {
        assert!(!agent_prime_permitted(false, true), "headless instance");
        assert!(!agent_prime_permitted(true, false), "unit test");
        assert!(!agent_prime_permitted(true, true));
        assert!(
            agent_prime_permitted(false, false),
            "a windowed aterm primes"
        );
    }

    #[test]
    fn a_pass_reports_exactly_what_it_did() {
        assert_eq!(
            run_agent_prime(false, false, None),
            AgentPrimeOutcome::Disabled
        );
        assert_eq!(
            run_agent_prime(true, false, None),
            AgentPrimeOutcome::NoHome
        );

        let home = aterm_tempfile::tempdir().expect("scratch home");
        std::fs::create_dir_all(home.path().join(".codex")).expect("detect codex");
        let AgentPrimeOutcome::Ran(pass) = crate::test_env::scoped_unset("XDG_CONFIG_HOME", || {
            run_agent_prime(true, false, Some(home.path().to_path_buf()))
        }) else {
            panic!("enabled with a home must run");
        };
        assert!(pass.changed(), "{}", pass.summary);
        assert_eq!(pass.agents.len(), 1);
        assert_eq!(pass.agents[0].agent, "codex");
        assert_eq!(pass.agents[0].outcome, aterm_primer::Outcome::Installed);
        let written = std::fs::read_to_string(home.path().join(".codex/AGENTS.md"))
            .expect("the primer landed");
        assert_eq!(written, aterm_primer::primer_block(Some("codex")));
        assert!(
            !home.path().join(".claude").exists(),
            "never creates an agent dir"
        );

        let dev_home = aterm_tempfile::tempdir().expect("scratch home");
        std::fs::create_dir_all(dev_home.path().join(".codex")).expect("detect codex");
        assert_eq!(
            run_agent_prime(true, true, Some(dev_home.path().to_path_buf())),
            AgentPrimeOutcome::NotReleaseBundle
        );
        assert!(
            !dev_home.path().join(".codex/AGENTS.md").exists(),
            "nothing is written from a non-release bundle"
        );
    }
}

#[cfg(all(test, windows))]
mod tab_shell_env_tests {
    #[test]
    fn a_shell_tab_is_told_its_shell_and_a_command_is_told_nothing() {
        let (key, cmd) = super::tab_shell_env(false, Some("cmd")).expect("a shell tab");
        assert_eq!(key, "ATERM_TAB_SHELL");
        assert!(
            std::path::Path::new(&cmd).is_absolute()
                && cmd.to_ascii_lowercase().ends_with("cmd.exe"),
            "{cmd}"
        );
        let (_, default) = super::tab_shell_env(false, None).expect("a default-shell tab");
        let (program, _) = aterm_pty::select_shell_with_origin(None);
        assert_eq!(default, program.to_string_lossy());
        assert_eq!(super::tab_shell_env(true, Some("cmd")), None);
        assert!(
            aterm_types::domain::is_ai_env_var(&key),
            "deny-listed at the hop"
        );
    }
}

#[cfg(test)]
mod spawn_shell_type_tests {
    use aterm_core::shell_integration::ShellType;
    use std::ffi::{OsStr, OsString};

    #[test]
    fn the_integration_is_the_resolved_programs() {
        let comspec = |hint: Option<&OsStr>| {
            assert_eq!(hint, None);
            OsString::from(r"C:\WINDOWS\system32\cmd.exe")
        };
        assert_eq!(super::spawn_shell_type(None, comspec), ShellType::Cmd);
        let git_bash = |hint: Option<&OsStr>| {
            assert_eq!(hint, Some(OsStr::new("gitbash")));
            OsString::from(r"C:\Program Files\Git\bin\bash.exe")
        };
        assert_eq!(
            super::spawn_shell_type(Some("gitbash"), git_bash),
            ShellType::Bash
        );
        let pwsh = |_: Option<&OsStr>| OsString::from(r"C:\Program Files\PowerShell\7\pwsh.exe");
        assert_eq!(super::spawn_shell_type(None, pwsh), ShellType::PowerShell);
    }

    #[test]
    fn the_default_tab_is_integrated_as_the_program_it_runs() {
        let (program, _) = aterm_pty::select_shell_with_origin(None);
        assert_eq!(
            super::detect_spawn_shell(None),
            ShellType::detect(&program.to_string_lossy())
        );
    }
}

#[cfg(test)]
mod colorfgbg_env_tests {
    #[test]
    fn colorfgbg_names_the_themes_background() {
        use aterm_types::Appearance;
        let pair = |a| super::colorfgbg_env(a);
        assert_eq!(pair(None), ("COLORFGBG".to_string(), "15;0".to_string()));
        assert_eq!(pair(Some(Appearance::Dark)).1, "15;0");
        assert_eq!(pair(Some(Appearance::Light)).1, "0;15");
    }
}

#[cfg(test)]
mod child_identity_env_tests {
    use super::{provision_child_identity_env, provision_child_recursion_env};
    use aterm_session::SessionId;
    use aterm_types::domain::{ENV_LAUNCH_NONCE, ENV_PARENT_SESSION_ID, ENV_SESSION_ID};

    fn keys(env: &[(String, String)]) -> Vec<&str> {
        env.iter().map(|(k, _)| k.as_str()).collect()
    }

    #[test]
    fn identity_env_carries_the_parent_id_and_the_multiplexer_baseline() {
        let parent = SessionId::generate();
        let env = provision_child_identity_env(&parent);
        assert_eq!(
            keys(&env),
            vec![ENV_PARENT_SESSION_ID, aterm_types::domain::ENV_MUX_BASE],
            "identity is WHO spawned it plus WHAT it was born into — both pure \
             facts, neither a capability"
        );
        assert_eq!(env[0].1, parent.as_str());
        let base = &env[1].1;
        assert!(
            base.contains('|'),
            "the baseline keeps both halves, got {base:?}"
        );
        assert_eq!(
            *base,
            format!(
                "{}|{}",
                std::env::var("TMUX").unwrap_or_default(),
                std::env::var("STY").unwrap_or_default()
            ),
            "the stamp is THIS process's signature at the moment of the spawn"
        );
    }

    #[test]
    fn controller_observe_env_is_the_origin_sid_only_and_one_hop() {
        use aterm_types::domain::{ENV_OBSERVE_SESSION_ID, is_ai_env_var};
        let origin = SessionId::generate();
        let env = super::provision_observe_env(&origin);
        assert_eq!(keys(&env), vec![ENV_OBSERVE_SESSION_ID]);
        assert_eq!(env[0].1, origin.as_str());
        assert!(
            is_ai_env_var(ENV_OBSERVE_SESSION_ID),
            "the hint must be stripped on inheritance — a grandchild is not the controller"
        );
    }

    #[test]
    fn identity_plus_recursion_covers_the_original_contract() {
        let parent = SessionId::generate();
        let mut env = provision_child_identity_env(&parent);
        let (rec, _prov) = provision_child_recursion_env();
        env.extend(rec);
        let k = keys(&env);
        for want in [ENV_PARENT_SESSION_ID, ENV_SESSION_ID, ENV_LAUNCH_NONCE] {
            assert!(k.contains(&want), "missing {want} after split");
        }
        let mut sorted = k.clone();
        sorted.sort_unstable();
        let before = sorted.len();
        sorted.dedup();
        assert_eq!(before, sorted.len(), "no key may be emitted twice");
    }
}

#[cfg(test)]
mod capability_secrecy_conformance {
    use super::{provision_child_proxy_from, provision_child_recursion_env};
    use aterm_spec::verify::validate_transition_tiered;
    use aterm_types::domain::ENV_EDGE_TOKENS;

    #[test]
    fn edge_secret_provisioning_conforms_to_capability_secrecy() {
        let model = aterm_models_effects::capability_secrecy_model();
        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let label = "CapabilitySecrecy(provision_child_proxy)";
        let dir = std::env::temp_dir().join(format!("aterm-f1-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch socket dir");
        for (what, sock_dir) in [("a private dir", Some(dir.as_path())), ("no dir", None)] {
            let minted = provision_child_recursion_env();
            let secrets = [
                minted.1.read.to_hex(),
                minted.1.write.to_hex(),
                minted.1.signal.to_hex(),
            ];
            let (env, _child) = provision_child_proxy_from(minted, sock_dir);
            let in_env = env
                .iter()
                .any(|(_, value)| secrets.iter().any(|secret| value.contains(secret.as_str())));
            let in_file = env
                .iter()
                .find(|(key, _)| key == ENV_EDGE_TOKENS)
                .and_then(|(_, path)| crate::proxy::read_edge_tokens(path))
                .is_some_and(|(r, w, s)| [r, w, s] == secrets);
            let published = if in_env { 2 } else { i64::from(in_file) };

            if published > 0 {
                let init = model.init_state();
                let mut provisioned = init.clone();
                provisioned.insert("published", published);
                let (ok, why) = validate_transition_tiered(
                    &model,
                    &[],
                    &init,
                    &provisioned,
                    Some("Provision"),
                    label,
                );
                assert!(
                    ok,
                    "{what}: the secrets were published on channel {published} (2 = the \
                     inheritable env), which the model does not admit — {why}"
                );
                let (admitted, _) = validate_transition_tiered(
                    &buggy,
                    &[],
                    &init,
                    &provisioned,
                    Some("Provision"),
                    "CapabilitySecrecy(Buggy=1)",
                );
                assert!(
                    !admitted,
                    "{what}: the env-channel mutant admitted the real step"
                );
            }
            assert_eq!(
                published,
                i64::from(sock_dir.is_some()),
                "{what}: the file channel iff there is a private dir to hold the file"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod deferred_reader_gate_tests {
    use super::DeferredReaderGate;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn prepared_reader_cannot_activate_before_infallible_release() {
        let gate = DeferredReaderGate::closed();
        let waiter = gate.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let waiter_stop = stop.clone();
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let join = std::thread::spawn(move || {
            let _ = tx.send(waiter.wait_until_released(&waiter_stop));
        });
        while gate.parked() == 0 {
            std::thread::yield_now();
        }
        assert!(
            rx.try_recv().is_err(),
            "closed gate must prevent pre-Commit activation"
        );
        gate.release();
        gate.release();
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(60)),
            Ok(true),
            "release is idempotent and requires no resource acquisition"
        );
        join.join().unwrap();
    }
}

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

/// [`attach_reader`] with the reader's posts going to `post` (a warm spare's link, `crate::spare::SpareLink::post`, or a spawn's [`WakeTarget`]).
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
    /// A [`WakePost`] onto this target.
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
    // A previous reader still winding down (a park that missed its deadline):
    // wait it out — two readers on one master interleave bytes and corrupt both
    // engines. Bounded in practice: the park already poked the wake pipe, so the
    // old thread is past its last blocking read.
    if let Some(join) = session.reader_join.take() {
        let _ = join.join();
    }
    {
        let (probe, shell) = (factory.program_probe, session.pid);
        crate::term_lock(&session.term)
            .set_program_probe(Some(Box::new(move |since_ms| probe(shell, since_ms))));
    }
    let (cast_epoch, cast_watermark) = {
        let rec = session.ctx.cast.lock().unwrap_or_else(|p| p.into_inner());
        (rec.epoch(), rec.watermark())
    };
    if let Some(old_writer) = session.temporal_writer_join.take() {
        let _ = old_writer.join();
    }
    let (temporal_tx, temporal_writer_join, spine_mark) = if factory.temporal_recording {
        let policy = crate::app_render::pty_resize_policy(session.master);
        let host_resyncs =
            crate::alt_screen_resync::host_resyncs(aterm_pty::conpty_host(session.master));
        let spine_mark = {
            let mut rec = session
                .ctx
                .temporal
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            rec.set_resize_policy(policy);
            rec.set_host_resyncs_alt_screen(host_resyncs);
            crate::temporal::SpineMark::attach(rec.spine_watermark())
        };
        let (tx, join) = spawn_temporal_writer(session.ctx.temporal.clone())?;
        (Some(tx), Some(join), spine_mark)
    } else {
        (None, None, crate::temporal::SpineMark::default())
    };
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

    let compress_tx = spawn_compress_worker(session.term.clone(), session.ctx.ui_waiting.clone());
    crate::term_lock(&session.term).set_compress_offload_active(compress_tx.is_some());

    let mut read_buf = Vec::new();
    read_buf
        .try_reserve_exact(65_536)
        .map_err(|error| format!("reserve PTY read buffer: {error}"))?;
    read_buf.resize(65_536, 0);
    let (wake_rd, wake_wr) = aterm_pty::make_wake_pipe().unwrap_or((-1, -1));
    reset_reader_latches(session);
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
            proc_watch: (factory.proc_watch)(session.pid),
        },
        timeline: session.ctx.timeline.clone(),
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
    session.temporal_writer_join = temporal_writer_join;
    session.reader_join = Some(reader);
    Ok(())
}

/// True when a parked/deferred session can be reattached without blocking the caller on either its old PTY reader or temporal writer.
#[must_use]
pub(crate) fn reader_attach_ready(session: &Session) -> bool {
    session
        .reader_join
        .as_ref()
        .is_none_or(std::thread::JoinHandle::is_finished)
        && session
            .temporal_writer_join
            .as_ref()
            .is_none_or(std::thread::JoinHandle::is_finished)
}

/// Whether a session's parser is behind its program because of an update's park (`SessionCtx::update_parked`).
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
    let Some(join) = session.reader_join.take() else {
        // Distinct branch.
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
    while !join.is_finished() {
        if Instant::now() >= deadline {
            session.reader_join = Some(join);
            return false;
        }
        crate::watchdog::beat(crate::watchdog::Breadcrumb::UpdateHandoff);
        std::thread::yield_now();
    }
    if join.join().is_err() {
        // Fail closed so automatic handoff rolls back and reattaches a reader;
        // never treat thread termination alone as a successful quiescence ACK.
        return false;
    }
    if let Some(writer) = session.temporal_writer_join.take() {
        while !writer.is_finished() {
            if Instant::now() >= deadline {
                session.temporal_writer_join = Some(writer);
                return false;
            }
            crate::watchdog::beat(crate::watchdog::Breadcrumb::UpdateHandoff);
            std::thread::yield_now();
        }
        drop(writer);
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

#[cfg(all(test, unix))]
#[path = "holder_image_reader_tests.rs"]
mod holder_image_reader_tests;

#[cfg(all(test, unix))]
mod shell_exit_tests {
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc;

    use super::attach_reader_inner;
    use crate::{App, Wake, WindowId};

    #[test]
    fn the_tab_ends_when_its_shell_exits_while_a_job_holds_the_terminal() {
        let app = App::headless_for_test();
        let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
        let mut shell = aterm_uds::spawnfd::command("/bin/sh")
            .args(["-c", "read _"])
            .stdin(aterm_uds::spawnfd::Stdio::piped())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .spawn()
            .expect("spawn the shell");
        let mut session = crate::stub_session(91);
        session.master = master;
        session.pid = i32::try_from(shell.id()).expect("pid fits i32");
        session.shell_identity = aterm_pty::record_spawned_shell(session.pid);
        let (posted, wakes) = mpsc::channel::<&'static str>();
        let post = std::sync::Arc::new(move |wake: Wake| {
            let _ = posted.send(match wake {
                Wake::Ready { .. } => "ready",
                Wake::Exit { .. } => "exit",
                _ => "other",
            });
            true
        });
        attach_reader_inner(&mut session, WindowId(0), post, &app.session_factory, None)
            .expect("attach the real reader");
        let patience = aterm_spec::wait::MUST_HAPPEN;
        let next = |what: &str| loop {
            match wakes.recv_timeout(patience) {
                Ok(w) if w == what => return true,
                Ok(_) => continue,
                Err(_) => return false,
            }
        };
        assert!(next("ready"), "the reader's gather started");
        // SAFETY: a write of a live buffer to the test-owned slave.
        let wrote = unsafe { libc::write(slave, b"bye".as_ptr().cast(), 3) };
        assert_eq!(wrote, 3, "write the shell's last words");
        drop(shell.stdin.take());
        let exited = next("exit");
        let _ = shell.wait();
        let row = |session: &crate::Session| {
            crate::term_lock(&session.term)
                .row_text(0)
                .unwrap_or_default()
        };
        let shown = row(&session);
        // SAFETY: a write of a live buffer to the test-owned slave.
        let wrote = unsafe { libc::write(slave, b"late".as_ptr().cast(), 4) };
        assert_eq!(wrote, 4, "write the job's later output");
        let mut late_shown = row(&session).starts_with("byelate");
        let until = std::time::Instant::now() + patience;
        while exited && !late_shown && std::time::Instant::now() < until {
            std::thread::yield_now();
            late_shown = row(&session).starts_with("byelate");
        }
        aterm_pty::close_fd(slave);
        let reader_ended = session.reader_join.as_ref().is_some_and(|reader| {
            let until = std::time::Instant::now() + patience;
            while !reader.is_finished() && std::time::Instant::now() < until {
                std::thread::yield_now();
            }
            reader.is_finished()
        });
        let _ = super::park_reader(
            &mut session,
            std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        );
        let exits_after = wakes.try_iter().filter(|wake| *wake == "exit").count();
        let master = std::mem::replace(&mut session.master, -1);
        aterm_pty::close_fd(master);
        assert!(
            exited,
            "the shell exited while a job held its terminal, and the tab stayed open"
        );
        assert!(
            shown.starts_with("bye"),
            "what the shell wrote before its exit reaches the screen: {shown:?}"
        );
        assert!(
            late_shown,
            "what the job wrote after the exit reaches the screen: {:?}",
            row(&session)
        );
        assert!(reader_ended, "the job let go, and the reader read on");
        assert_eq!(exits_after, 0, "the job's letting go is not a second exit");
    }

    /// A real session on a real pseudo-terminal whose slave the test holds, over a real shell (`read _; exit 3`, its input piped), the reader attached: each `Wake::Exit` it posts is sent on with whether the shell was collectable then.
    fn exit_rig(
        app: &App,
        id: u64,
    ) -> (
        crate::Session,
        i32,
        aterm_uds::spawnfd::Child,
        mpsc::Receiver<bool>,
    ) {
        let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
        let shell = sh("read _; exit 3");
        let pid = shell.id();
        let mut session = crate::stub_session(id);
        session.master = master;
        session.pid = i32::try_from(pid).expect("pid fits i32");
        session.shell_identity = aterm_pty::record_spawned_shell(session.pid);
        let (ready_tx, ready) = mpsc::channel::<()>();
        let (exit_tx, exits) = mpsc::channel::<bool>();
        let post = std::sync::Arc::new(move |wake: Wake| {
            match wake {
                Wake::Ready { .. } => {
                    let _ = ready_tx.send(());
                }
                Wake::Exit { .. } => {
                    let _ = exit_tx.send(collectable(pid));
                }
                _ => {}
            }
            true
        });
        attach_reader_inner(&mut session, WindowId(0), post, &app.session_factory, None)
            .expect("attach the real reader");
        // A hang detector: the gather's start is the event.
        ready
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the reader's gather started");
        (session, slave, shell, exits)
    }

    /// Park the rig's reader and free its master.
    fn end_rig(mut session: crate::Session) {
        let _ = super::park_reader(
            &mut session,
            std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        );
        let master = std::mem::replace(&mut session.master, -1);
        aterm_pty::close_fd(master);
    }

    /// The gather posts a shell's exit at its terminal's EOF only once the shell is retired ([`super::ShellExit::await_retired_at_exit`]): the terminal closes while the shell runs, the shell exits 300 ms later, and the `Wake::Exit` finds it collectable (at 20 ms, a starved shell that failed at start read as a clean exit). NEGATIVE CONTROL: the same look at the running shell finds it not collectable.
    #[test]
    fn the_gather_posts_the_exit_at_its_terminals_eof_only_once_the_shell_is_retired() {
        let app = App::headless_for_test();
        let (session, slave, mut shell, exits) = exit_rig(&app, 95);
        assert!(
            !collectable(shell.id()),
            "a running shell is not collectable"
        );
        // The shell closes its terminal on its way out; its exit trails.
        aterm_pty::close_fd(slave);
        let stdin = shell.stdin.take().expect("its input");
        let closer = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            drop(stdin);
        });
        // A hang detector: the exit's post is the event.
        let retired = exits.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
        closer.join().expect("the closer");
        // Reaped here, before the session goes (its drop takes its shell).
        let code = shell.wait().expect("its owner reaps it").code();
        end_rig(session);
        assert_eq!(
            retired,
            Ok(true),
            "the exit was posted before the shell could be collected"
        );
        assert_eq!(code, Some(3), "and the status was left for its owner");
    }

    /// A cancellation wake may precede the stop flag. Both exit paths must
    /// send Wake even then, including when the exit watch is ready too.
    #[test]
    fn a_cancelled_retirement_wait_wakes_the_reader_without_posting_a_shell_exit() {
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

        use super::{GatherMsg, ShellExit, ShellRetirement};

        let pipe = || {
            let (rd, wr) = aterm_pty::make_wake_pipe().expect("owned wake pipe");
            // SAFETY: both fresh descriptors are transferred to one owner each.
            unsafe { (OwnedFd::from_raw_fd(rd), OwnedFd::from_raw_fd(wr)) }
        };
        let (wake_rd, wake_wr) = pipe();
        aterm_pty::wake(wake_wr.as_raw_fd());
        for (watched, watch_ready) in [(false, false), (true, false), (true, true)] {
            let (watch_rd, watch_wr) = pipe();
            if watch_ready {
                aterm_pty::wake(watch_wr.as_raw_fd());
            }
            let exit = ShellExit {
                watch: watched.then_some(watch_rd),
                // This process is not its own child; no shell is needed.
                pid: std::process::id(),
            };
            let result = exit.await_retired_at_exit(wake_rd.as_raw_fd(), &NO_STOP);
            assert_eq!(result, ShellRetirement::Cancelled);
            assert!(!NO_STOP.load(std::sync::atomic::Ordering::Acquire));
            for ending in [GatherMsg::Exited, GatherMsg::Eof] {
                assert!(matches!(result.gather_message(ending), GatherMsg::Wake));
            }
        }
        for watched in [false, true] {
            let (watch_rd, _watch_wr) = pipe();
            let exit = ShellExit {
                watch: watched.then_some(watch_rd),
                pid: std::process::id(),
            };
            assert_eq!(
                exit.await_retired_at_exit(-1, &STOPPED),
                ShellRetirement::Cancelled,
                "the stop flag also cancels without a wake pipe"
            );
            assert_eq!(
                exit.await_retired(std::time::Duration::ZERO, -1, &NO_STOP),
                ShellRetirement::Unconfirmed,
                "timeout or an unwaitable pid retains the existing exit policy"
            );
        }
        for result in [ShellRetirement::Retired, ShellRetirement::Unconfirmed] {
            assert!(matches!(
                result.gather_message(GatherMsg::Exited),
                GatherMsg::Exited
            ));
            assert!(matches!(
                result.gather_message(GatherMsg::Eof),
                GatherMsg::Eof
            ));
        }
    }

    #[test]
    fn waiting_for_the_shells_retirement_reaps_nothing() {
        let mut shell = aterm_uds::spawnfd::command("/bin/sh")
            .args(["-c", "read _; exit 3"])
            .stdin(aterm_uds::spawnfd::Stdio::piped())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .spawn()
            .expect("spawn the shell");
        let pid = shell.id();
        let exit = super::ShellExit {
            watch: Some(aterm_uds::exitwatch::exit_fd(pid).expect("the shell is watched")),
            pid,
        };
        exit.await_retired(std::time::Duration::from_millis(1), -1, &NO_STOP);
        assert!(
            shell.try_wait().expect("a look at the shell").is_none(),
            "a shell still running is left running"
        );
        drop(shell.stdin.take());
        exit.await_retired(aterm_spec::wait::MUST_HAPPEN, -1, &NO_STOP);
        // SAFETY: a zeroed `siginfo_t` out-param for a non-reaping peek.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: `WNOWAIT` leaves the child waitable; `WNOHANG` never blocks.
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                pid,
                &mut info,
                libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
            )
        };
        assert_eq!(rc, 0, "waitid on our own child");
        // SAFETY: `waitid` filled the siginfo (or left it zeroed).
        let collectable = unsafe { info.si_pid() };
        assert_eq!(
            u32::try_from(collectable).ok(),
            Some(pid),
            "collectable the moment the wait ends"
        );
        assert_eq!(
            shell.wait().expect("its owner reaps it").code(),
            Some(3),
            "and the status was left for its owner"
        );
    }

    /// Whether `pid`, a child of this process, is collectable now: a look that
    /// reaps nothing (`WNOWAIT`).
    fn collectable(pid: u32) -> bool {
        // SAFETY: a zeroed `siginfo_t` out-param for a non-reaping peek.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: `WNOWAIT` leaves the child waitable; `WNOHANG` never blocks.
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                pid,
                &mut info,
                libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
            )
        };
        // SAFETY: `waitid` filled the siginfo (or left it zeroed).
        rc == 0 && u32::try_from(unsafe { info.si_pid() }).ok() == Some(pid)
    }

    /// THE WAIT FOR THE SHELL'S RETIREMENT IS A HANG DETECTOR
    /// ([`super::SHELL_RETIRE_HANG`]): a shell whose exit trails the moment
    /// the reader looks — a starved machine's moment between a session
    /// leader's terminal closing on its way out and its exit, measured past
    /// the 20 ms this wait was — is waited for and left collectable, so the
    /// exit path reads its real status, never none (a failed shell read as a
    /// clean exit, its only pane closed). NEGATIVE CONTROL: a stop's wake ends
    /// the wait at once, the shell still running; a wait the stop could not
    /// end would hold a closing tab's reader for the whole bound.
    #[test]
    fn the_wait_for_a_shells_retirement_outlasts_a_slow_exit_and_a_stop_ends_it() {
        let mut shell = aterm_uds::spawnfd::command("/bin/sh")
            .args(["-c", "read _; exit 3"])
            .stdin(aterm_uds::spawnfd::Stdio::piped())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .spawn()
            .expect("spawn the shell");
        let pid = shell.id();
        let exit = super::ShellExit {
            watch: Some(aterm_uds::exitwatch::exit_fd(pid).expect("the shell is watched")),
            pid,
        };
        // Negative control: the stop's wake, already written, ends a wait far
        // longer than this test's hang detector.
        let mut wake = [0; 2];
        // SAFETY: a two-int out-param for `pipe(2)`.
        assert_eq!(unsafe { libc::pipe(wake.as_mut_ptr()) }, 0, "the wake pipe");
        // SAFETY: one byte from a live buffer into the pipe's write end.
        assert_eq!(unsafe { libc::write(wake[1], [1u8].as_ptr().cast(), 1) }, 1);
        let (ended_tx, ended) = std::sync::mpsc::channel();
        let wake_rd = wake[0];
        let waiter = std::thread::spawn(move || {
            exit.await_retired(std::time::Duration::from_secs(600), wake_rd, &NO_STOP);
            let _ = ended_tx.send(());
            exit
        });
        // A hang detector: the wake is the event.
        ended
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the stop's wake ended the wait");
        let exit = waiter.join().expect("the waiter");
        aterm_pty::close_fd(wake[0]);
        aterm_pty::close_fd(wake[1]);
        assert!(
            shell.try_wait().expect("a look at the shell").is_none(),
            "a stop leaves the shell running"
        );
        // The shell's exit trails the wait's start: its input closes a
        // moment after the reader began to wait.
        let stdin = shell.stdin.take().expect("its input");
        let closer = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            drop(stdin);
        });
        exit.await_retired_at_exit(-1, &NO_STOP);
        // Looked at the moment the wait ends, before anything else waits.
        let retired = collectable(pid);
        closer.join().expect("the closer");
        assert!(retired, "the wait outlasted the shell's slow exit");
        assert_eq!(
            shell.wait().expect("its owner reaps it").code(),
            Some(3),
            "and the status was left for its owner"
        );
    }

    /// `/bin/sh -c <script>` with its input piped and its output dropped.
    fn sh(script: &str) -> aterm_uds::spawnfd::Child {
        aterm_uds::spawnfd::command("/bin/sh")
            .args(["-c", script])
            .stdin(aterm_uds::spawnfd::Stdio::piped())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .spawn()
            .expect("spawn the shell")
    }

    /// A gather's stop never raised, and one raised.
    static NO_STOP: AtomicBool = AtomicBool::new(false);
    static STOPPED: AtomicBool = AtomicBool::new(true);

    /// `exit.await_retired(within, wake_rd, stop)` on a thread, and whether it
    /// ended within the hang detector.
    fn ended_at_once(
        exit: super::ShellExit,
        within: std::time::Duration,
        wake_rd: i32,
        stop: &'static AtomicBool,
    ) -> bool {
        let (ended_tx, ended) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            exit.await_retired(within, wake_rd, stop);
            let _ = ended_tx.send(());
        });
        // A hang detector: the wait's end is the event.
        ended.recv_timeout(aterm_spec::wait::MUST_HAPPEN).is_ok()
    }

    /// A SHELL ALREADY ON ITS WAY OUT WHEN ITS READER ATTACHES KEEPS ITS WAIT:
    /// Darwin's kqueue refuses an exit watch on a process that has begun to
    /// exit (`ESRCH`) before that process can be collected, so a shell of this
    /// process's own that failed at once, already exiting when a loaded machine
    /// attached its reader, keeps a [`super::ShellExit`] with no watch
    /// ([`super::shell_exit_of`]), and the wait for it ends at once once it is
    /// collectable. Before, it had none, and the exit was posted at the master's
    /// EOF before its status could be read: a shell killed at start read as a
    /// clean exit, its only pane closed. NEGATIVE CONTROL: an adopted shell no
    /// watch can be made for has no wait. Darwin's alone: Linux's pidfd watches
    /// a zombie, so there such a shell is watched.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn a_shell_already_exiting_when_its_reader_attaches_keeps_its_wait() {
        // A shell already exited: the kernel takes no watch of it, and this
        // process can still wait for it.
        let mut gone = sh("exit 3");
        let pid = gone.id();
        let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        while !collectable(pid) {
            // A hang detector: the shell's exit is the event.
            assert!(std::time::Instant::now() < deadline, "the shell exited");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(
            aterm_uds::exitwatch::exit_fd(pid).is_none(),
            "the kernel watches no process that has exited"
        );
        let own = i32::try_from(pid).expect("a pid");
        let kept = super::shell_exit_of(aterm_pty::ShellIdentity::from_parts(own, None, true))
            .expect("a shell of this process's own keeps its wait");
        assert!(kept.watch.is_none(), "with no watch");
        assert!(
            super::shell_exit_of(aterm_pty::ShellIdentity::from_parts(own, None, false)).is_none(),
            "an adopted shell no watch can be made for has no wait"
        );
        assert!(
            ended_at_once(kept, super::SHELL_RETIRE_HANG, -1, &NO_STOP),
            "a shell already collectable is not waited for"
        );
        assert_eq!(gone.wait().expect("its owner reaps it").code(), Some(3));
    }

    /// A SHELL WITH NO EXIT WATCH IS STILL WAITED FOR ([`super::shell_exit_of`]
    /// keeps one for a shell of this process's own the kernel would not watch):
    /// its exit trails the wait's start, and the gather's wait
    /// ([`super::ShellExit::await_retired_at_exit`]) ends only once it is
    /// collectable. NEGATIVE CONTROLS: the stop's wake ends a wait with no
    /// watch at once, the shell still running; and a pid that is not this
    /// process's child ends it at once.
    #[test]
    fn a_shell_that_could_not_be_watched_is_still_waited_for() {
        let mut shell = sh("read _; exit 3");
        let pid = shell.id();
        let unwatched = || super::ShellExit { watch: None, pid };
        let mut wake = [0; 2];
        // SAFETY: a two-int out-param for `pipe(2)`.
        assert_eq!(unsafe { libc::pipe(wake.as_mut_ptr()) }, 0, "the wake pipe");
        // SAFETY: one byte from a live buffer into the pipe's write end.
        assert_eq!(unsafe { libc::write(wake[1], [1u8].as_ptr().cast(), 1) }, 1);
        assert!(
            ended_at_once(
                unwatched(),
                std::time::Duration::from_secs(600),
                wake[0],
                &NO_STOP
            ),
            "the stop's wake ended the wait"
        );
        aterm_pty::close_fd(wake[0]);
        aterm_pty::close_fd(wake[1]);
        let parent = super::ShellExit {
            watch: None,
            pid: std::os::unix::process::parent_id(),
        };
        assert!(
            ended_at_once(parent, std::time::Duration::from_secs(600), -1, &NO_STOP),
            "a pid this process cannot wait for is not waited for"
        );
        assert!(
            shell.try_wait().expect("a look at the shell").is_none(),
            "the shell is still running"
        );
        let stdin = shell.stdin.take().expect("its input");
        let closer = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            drop(stdin);
        });
        unwatched().await_retired_at_exit(-1, &NO_STOP);
        // Looked at the moment the wait ends, before anything else waits.
        let retired = collectable(pid);
        closer.join().expect("the closer");
        assert!(
            retired,
            "the wait outlasted the unwatched shell's slow exit"
        );
        assert_eq!(
            shell.wait().expect("its owner reaps it").code(),
            Some(3),
            "and the status was left for its owner"
        );
    }

    /// A GATHER WITH NO WAKE PIPE (fd exhaustion: `wake_rd == -1`) IS STOPPED
    /// BY ITS FLAG ALONE, and its wait for the shell's retirement reads it: a
    /// raised stop ends the minute-long wait at once, the shell still running,
    /// whether the shell is watched (the watch's poll, read in steps) or not
    /// (the naps). Before, that wait read no flag, and a park or a tab's close
    /// held its reader for the whole bound. NEGATIVE CONTROL: with the stop
    /// down, a wait bounded at 300 ms on the same running shell lasts its
    /// bound.
    #[test]
    fn a_stop_with_no_wake_pipe_ends_the_wait_for_a_shells_retirement() {
        let mut shell = sh("read _; exit 3");
        let pid = shell.id();
        let watched = || super::ShellExit {
            watch: Some(aterm_uds::exitwatch::exit_fd(pid).expect("the shell is watched")),
            pid,
        };
        let unwatched = || super::ShellExit { watch: None, pid };
        // A bound far past the hang detector's minute: only the stop can end these waits in it.
        let far = std::time::Duration::from_secs(600);
        assert!(
            ended_at_once(watched(), far, -1, &STOPPED),
            "a raised stop ended the watched wait"
        );
        assert!(
            ended_at_once(unwatched(), far, -1, &STOPPED),
            "a raised stop ended the unwatched wait"
        );
        for exit in [watched(), unwatched()] {
            let bound = std::time::Duration::from_millis(300);
            let began = std::time::Instant::now();
            exit.await_retired(bound, -1, &NO_STOP);
            assert!(
                began.elapsed() >= bound,
                "with the stop down the wait lasts its bound (watched: {})",
                exit.watch.is_some()
            );
        }
        assert!(
            shell.try_wait().expect("a look at the shell").is_none(),
            "the shell is still running"
        );
        drop(shell.stdin.take());
        assert_eq!(shell.wait().expect("its owner reaps it").code(), Some(3));
    }
}

#[cfg(test)]
mod park_reader_tests {
    use super::park_reader;

    #[test]
    fn panicked_reader_fails_closed_instead_of_authorizing_handoff() {
        let mut session = crate::stub_session(77);
        session.reader_join = Some(std::thread::spawn(|| {
            panic!("injected PTY reader failure");
        }));
        let waits = crate::session_watch::Subscription::single(&session.ctx.watchers);

        assert!(!park_reader(
            &mut session,
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        ));
        assert!(session.reader_join.is_none());
        assert!(session.ctx.update_parked.is_parked());
        assert!(
            waits.wait(std::time::Duration::ZERO),
            "the park rings the session's waits"
        );
        assert!(park_reader(
            &mut session,
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        ));
        assert!(!waits.wait(std::time::Duration::ZERO));
    }

    #[test]
    fn a_reattached_reader_never_inherits_a_dead_wake_arm() {
        let session = crate::stub_session(78);
        session
            .output_wake_pending
            .store(1_000, std::sync::atomic::Ordering::Relaxed);
        session
            .reader_stop
            .store(true, std::sync::atomic::Ordering::Release);
        session.ctx.update_parked.park();
        let waits = crate::session_watch::Subscription::single(&session.ctx.watchers);

        super::reset_reader_latches(&session);

        assert!(
            session.ctx.update_parked.is_parked(),
            "the attach ends no park: the new reader has parsed nothing yet \
             (`update_park_catch_up_tests`)"
        );
        assert!(
            !waits.wait(std::time::Duration::ZERO),
            "the attach resumes nothing, so it rings nobody: the caught-up \
             reader rings as it ends the park"
        );

        assert!(
            !session
                .reader_stop
                .load(std::sync::atomic::Ordering::Acquire),
            "a fresh reader must not start already stopped"
        );
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        super::gated_output_wake(
            &session.output_wake_pending,
            1_001,
            0,
            false,
            || tx.send(()).is_ok(),
            || 1_002,
        );
        assert_eq!(
            rx.try_iter().count(),
            1,
            "the first burst after a re-attach must post its wake"
        );
    }

    #[test]
    fn a_park_past_its_deadline_keeps_the_unfinished_handle() {
        let mut session = crate::stub_session(79);
        let (finish_reader, reader_held) = std::sync::mpsc::channel::<()>();
        session.reader_join = Some(std::thread::spawn(move || {
            let _ = reader_held.recv();
        }));
        assert!(!park_reader(&mut session, std::time::Instant::now()));
        assert!(session.reader_join.is_some(), "the reader's handle is kept");
        assert!(!super::reader_attach_ready(&session));
        assert!(session.ctx.update_parked.is_parked());
        drop(finish_reader);
        assert!(park_reader(
            &mut session,
            std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        ));
        assert!(session.reader_join.is_none());
        assert!(super::reader_attach_ready(&session));

        let reader = std::thread::spawn(|| {});
        let stopped = aterm_spec::wait::within(
            aterm_spec::wait::MUST_HAPPEN,
            std::time::Duration::ZERO,
            || reader.is_finished().then_some(()),
        );
        assert!(stopped.is_some(), "timed out waiting for the reader to end");
        session.reader_join = Some(reader);
        let (finish_writer, writer_held) = std::sync::mpsc::channel::<()>();
        session.temporal_writer_join = Some(std::thread::spawn(move || {
            let _ = writer_held.recv();
        }));
        assert!(!park_reader(&mut session, std::time::Instant::now()));
        assert!(
            session.reader_join.is_none(),
            "the stopped reader is joined"
        );
        assert!(
            session.temporal_writer_join.is_some(),
            "the writer's handle is kept"
        );
        assert!(!super::reader_attach_ready(&session));
        drop(finish_writer);
        let joined = aterm_spec::wait::within(
            aterm_spec::wait::MUST_HAPPEN,
            std::time::Duration::ZERO,
            || super::reader_attach_ready(&session).then_some(()),
        );
        assert!(joined.is_some(), "timed out waiting for the writer to end");
    }
}

#[cfg(all(test, target_os = "macos"))]
mod update_park_catch_up_tests {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::time::Instant;

    use aterm_core::terminal::BlockState;

    use super::{DeferredReaderGate, UpdatePark, attach_reader_inner, park_reader};
    use crate::{App, Session, Wake, WindowId};

    const PATIENCE: std::time::Duration = aterm_spec::wait::MUST_HAPPEN;

    fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
        let seen =
            aterm_spec::wait::within(PATIENCE, std::time::Duration::ZERO, || done().then_some(()));
        assert!(seen.is_some(), "timed out waiting for {what}");
    }

    fn newest_block(session: &Session) -> Option<BlockState> {
        crate::term_lock(&session.term)
            .all_blocks()
            .last()
            .map(|block| block.state)
    }

    fn program_writes(slave: i32, bytes: &[u8]) {
        // SAFETY: a write of a live buffer to the test-owned slave.
        let wrote = unsafe { libc::write(slave, bytes.as_ptr().cast(), bytes.len()) };
        assert_eq!(
            usize::try_from(wrote).ok(),
            Some(bytes.len()),
            "the program wrote"
        );
    }

    fn parked_at_a_prompt(app: &App, id: u64) -> (Session, i32) {
        let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
        let mut session = crate::stub_session(id);
        session.master = master;
        attach_reader_inner(
            &mut session,
            WindowId(0),
            Arc::new(|_: Wake| true),
            &app.session_factory,
            None,
        )
        .expect("attach the real reader");
        program_writes(slave, b"\x1b]133;A\x07$ ");
        wait_for("the prompt to be parsed", || {
            newest_block(&session) == Some(BlockState::PromptOnly)
        });
        assert!(!session.ctx.update_parked.is_parked(), "a live reader");
        assert!(park_reader(&mut session, Instant::now() + PATIENCE));
        assert!(session.ctx.update_parked.is_parked());
        (session, slave)
    }

    fn close(mut session: Session, slave: i32) {
        let _ = park_reader(&mut session, Instant::now() + PATIENCE);
        let master = std::mem::replace(&mut session.master, -1);
        aterm_pty::close_fd(slave);
        aterm_pty::close_fd(master);
    }

    #[test]
    fn a_resumed_reader_ends_the_park_only_once_its_parser_holds_what_queued() {
        let app = App::headless_for_test();
        let (mut session, slave) = parked_at_a_prompt(&app, 93);
        program_writes(slave, b"\x1b]133;B\x07make\r\n\x1b]133;C\x07building...");
        assert!(aterm_pty::output_queue_len(session.master).is_some_and(|n| n > 0));

        let mut factory = app.session_factory.clone();
        factory.byte_arm = false;
        let gate = DeferredReaderGate::closed();
        attach_reader_inner(
            &mut session,
            WindowId(0),
            Arc::new(|_: Wake| true),
            &factory,
            Some(gate.clone()),
        )
        .expect("reattach the reader");
        assert!(
            session.ctx.update_parked.is_parked(),
            "attached, the reader has read nothing: the park stands"
        );

        session.last_output_ns.store(0, Ordering::Release);
        let stalled = session.term.lock().expect("terminal lock");
        gate.release();
        wait_for("the parse stage to take up what queued", || {
            session.last_output_ns.load(Ordering::Acquire) != 0
        });
        assert_eq!(aterm_pty::output_queue_len(session.master), Some(0));
        assert!(
            session.ctx.update_parked.is_parked(),
            "read but not yet parsed: the parser still shows the prompt"
        );
        assert_eq!(
            stalled.all_blocks().last().map(|block| block.state),
            Some(BlockState::PromptOnly)
        );
        drop(stalled);

        wait_for("the caught-up reader to end the park", || {
            !session.ctx.update_parked.is_parked()
        });
        assert_eq!(
            newest_block(&session),
            Some(BlockState::Executing),
            "the park ended with `make` parsed"
        );
        close(session, slave);
    }

    #[test]
    fn a_resumed_reader_with_nothing_queued_ends_the_park_at_once() {
        let app = App::headless_for_test();
        let (mut session, slave) = parked_at_a_prompt(&app, 94);
        assert_eq!(aterm_pty::output_queue_len(session.master), Some(0));
        let waits = crate::session_watch::Subscription::single(&session.ctx.watchers);
        attach_reader_inner(
            &mut session,
            WindowId(0),
            Arc::new(|_: Wake| true),
            &app.session_factory,
            None,
        )
        .expect("reattach the reader");
        wait_for("the reader to end the park", || {
            !session.ctx.update_parked.is_parked()
        });
        assert!(waits.wait(PATIENCE), "the resume rings the session's waits");
        assert_eq!(newest_block(&session), Some(BlockState::PromptOnly));

        assert!(park_reader(&mut session, Instant::now() + PATIENCE));
        assert!(
            waits.wait(std::time::Duration::ZERO),
            "the park rings the session's waits"
        );
        let gate = DeferredReaderGate::closed();
        let (started, ready) = std::sync::mpsc::channel();
        attach_reader_inner(
            &mut session,
            WindowId(0),
            Arc::new(move |wake: Wake| {
                if matches!(wake, Wake::Ready { .. }) {
                    let _ = started.send(());
                }
                true
            }),
            &app.session_factory,
            Some(gate.clone()),
        )
        .expect("reattach the reader");
        session.ctx.update_parked.park_for_test();
        gate.release();
        assert!(
            ready.recv_timeout(PATIENCE).is_ok(),
            "timed out waiting for the reader to start"
        );
        session.reader_stop.store(true, Ordering::Release);
        aterm_pty::wake(session.wake_wr);
        wait_for("the reader to stop", || {
            session
                .reader_join
                .as_ref()
                .is_some_and(std::thread::JoinHandle::is_finished)
        });
        assert!(
            session.ctx.update_parked.is_parked(),
            "a stale catch-up ends no park"
        );
        assert!(
            !waits.wait(std::time::Duration::ZERO),
            "a stale catch-up rings nobody"
        );
        close(session, slave);
    }

    #[test]
    fn a_late_catch_up_never_ends_a_later_park() {
        let park = UpdatePark::default();
        assert!(!park.is_parked());
        park.park();
        let first = park.mark();
        park.park();
        assert!(!park.caught_up(first), "a stale mark ends nothing");
        assert!(park.is_parked(), "the later park stands");
        assert!(
            park.caught_up(park.mark()),
            "the current mark ends its park"
        );
        assert!(!park.is_parked());
        park.park();
        assert!(!park.caught_up(0));
        assert!(park.is_parked());
    }
}

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

/// This process's alt-screen archive origin, stamped on every live session's archive ([`new_live_terminal`]) and printed by `offscreen origin=` and the turn ledger's `arch=<origin>:<last>` marks.
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

/// What aterm, running outside any sandbox, does at one session's program's request, decided once, at spawn, by [`host_reach`] from the containment mode its shell runs under.
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

/// What this process's sessions reach on their programs' behalf ([`host_reach`] for the containment mode they run under).
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
    witness: &Arc<crate::spare::Witness>,
) {
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
    if factory.allow_kitty_file_transfer {
        configure_kitty_file_transfer(term, reach);
    }
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

fn configure_notifications(
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

/// Maximum bytes a Kitty non-direct medium may supply (matches the engine's `MAX_KITTY_IMAGE_BYTES`).
pub(crate) const MAX_KITTY_MEDIUM_BYTES: u64 = 4 * 1024 * 1024;

fn configure_kitty_file_transfer(term: &Arc<Mutex<Terminal>>, reach: HostReach) {
    if reach == HostReach::Refused {
        return;
    }
    term_lock(term).set_kitty_file_resolver(crate::host_io_prefetch::take_kitty_medium);
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

/// Consume a medium the engine took ([`read_kitty_medium`]'s read of it, `read` whether that read returned bytes).
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
            .custom_flags(libc::O_NONBLOCK)
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
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    (&file)
        .take(MAX_KITTY_MEDIUM_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_KITTY_MEDIUM_BYTES {
        // Distinct branch.
        return None;
    }
    Some(bytes)
}

const KITTY_TEMP_FILE_MARKER: &str = "tty-graphics-protocol";

fn consume_kitty_temp_file(path: &Path) {
    consume_kitty_temp_file_with(path, || {});
}

fn consume_kitty_temp_file_with(path: &Path, before_unlink: impl FnOnce()) {
    // Distinct branch.
    if let (Some(dir), Some(name)) = (path.parent(), path.file_name()) {
        // Distinct branch.
        consume_kitty_temp_entry(dir, name, before_unlink); // the client's temp file
    }
}

fn kitty_temp_entry_deletable(entry: &Path) -> bool {
    entry.to_string_lossy().contains(KITTY_TEMP_FILE_MARKER)
        && kitty_temp_dirs().iter().any(|tmp| entry.starts_with(tmp))
}

#[cfg(unix)]
fn consume_kitty_temp_entry(dir: &Path, name: &std::ffi::OsStr, before_unlink: impl FnOnce()) {
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
    if !kitty_temp_entry_deletable(&dir.join(name)) {
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
fn consume_kitty_temp_entry(dir: &Path, name: &std::ffi::OsStr, before_unlink: impl FnOnce()) {
    let Ok(dir) = std::fs::canonicalize(dir) else {
        return;
    };
    let entry = dir.join(name);
    if kitty_temp_entry_deletable(&entry) {
        before_unlink();
        let _ = std::fs::remove_file(entry);
    }
}

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
    // SAFETY: `cname` is a valid NUL-terminated C string; `shm_open` with O_RDONLY
    // either returns a valid fd or -1, which we check.
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
        // SAFETY: `addr`/`len` describe a valid read-only mapping just established;
        // copy the bytes out before unmapping.
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

const TEMPORAL_QUEUE_CAP: usize = 1024;

fn spawn_temporal_writer(
    temporal: Arc<std::sync::Mutex<crate::temporal::TemporalRecorder>>,
) -> Result<(crate::temporal::TemporalTx, std::thread::JoinHandle<()>), String> {
    let (temporal_tx, temporal_rx) = crate::temporal::queue(TEMPORAL_QUEUE_CAP);
    let join = crate::qos::spawn_lane_thread(
        "aterm-temporal-writer",
        crate::qos::Role::Responsive,
        move || {
            while let Some(msg) = temporal_rx.recv() {
                crate::temporal::write(&temporal, msg);
            }
        },
    )
    .map_err(|error| format!("spawn temporal writer: {error}"))?;
    Ok((temporal_tx, join))
}

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

    /// [`Self::submit`] for a report the engine sent unasked, which the writer hands to its [`ReportRoute`] instead of writing it as a reply.
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

/// Test seam: a real reply writer over `sink`, for the egress-contract tests that drive replies and pastes through one session's sink.
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
mod report_route_tests {
    use super::{ReportRoute, new_live_terminal, spawn_reply_writer};
    use aterm_session::sink::SinkWriter;
    use aterm_spec::wait::{MUST_HAPPEN, within};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn sink() -> Arc<SinkWriter> {
        Arc::new(SinkWriter::new(0x7fff_0f0f))
    }

    #[test]
    fn a_report_goes_the_input_way() {
        let term = Arc::new(Mutex::new(new_live_terminal(24, 80, None, None)));
        let modes = Arc::clone(crate::term_lock(&term).mode_mirror());
        let sink = sink();
        let before = sink.driven_input();
        let (lane, _join) = spawn_reply_writer(sink.clone(), ReportRoute::of(&term, &modes))
            .expect("the reply writer");
        assert!(lane.submit_report(Arc::from(&b"\x1b[48;30;100;600;900t"[..])));
        let driven = within(MUST_HAPPEN, Duration::from_millis(1), || {
            (sink.driven_input() != before).then_some(())
        });
        assert!(driven.is_some(), "the report was never input");
    }

    #[test]
    fn a_reply_and_a_routeless_report_are_not_input() {
        for reply in [true, false] {
            let sink = sink();
            let (epoch, driven) = (sink.input_epoch(), sink.driven_input());
            let (lane, _join) =
                spawn_reply_writer(sink.clone(), ReportRoute::default()).expect("the reply writer");
            let bytes = Arc::from(&b"\x1b[1;1R"[..]);
            assert!(if reply {
                lane.submit(bytes)
            } else {
                lane.submit_report(bytes)
            });
            let written = within(MUST_HAPPEN, Duration::from_millis(1), || {
                (sink.input_epoch() != epoch).then_some(())
            });
            assert!(written.is_some(), "the writer wrote it");
            assert_eq!(sink.driven_input(), driven, "not input anyone drove");
        }
    }
}

#[cfg(all(test, unix))]
mod reply_writer_lifetime_tests {
    use super::{ReportRoute, spawn_reply_writer};
    use aterm_session::sink::SinkWriter;
    use std::io::Read as _;
    use std::os::unix::net::UnixStream;
    use std::sync::Arc;
    use std::time::Duration;

    use aterm_spec::wait::MUST_HAPPEN;

    fn sink_and_peer() -> (Arc<SinkWriter>, UnixStream) {
        let (peer, writer) = UnixStream::pair().expect("socketpair");
        peer.set_read_timeout(Some(MUST_HAPPEN))
            .expect("read timeout");
        writer.set_nonblocking(true).expect("O_NONBLOCK");
        let owned: std::os::fd::OwnedFd = writer.into();
        let sink = SinkWriter::new_owned(owned);
        sink.note_master_nonblocking(true);
        (Arc::new(sink), peer)
    }

    const SPILL_FRAME: usize = 1 << 20;

    fn spill_is_delivered(sink: &SinkWriter, peer: &mut UnixStream) {
        let frame: Vec<u8> = (0..=250u8).cycle().take(SPILL_FRAME).collect();
        let receipt = sink
            .write_frame_interactive_with_receipt(&frame)
            .expect("the frame is accepted, its tail spilled");
        assert_eq!(receipt.accepted(), frame.len(), "the whole frame is taken");
        assert!(
            sink.spilled_bytes() > 0,
            "the frame must spill, or the arranger is never asked"
        );
        let mut got = vec![0u8; frame.len()];
        peer.read_exact(&mut got)
            .expect("every spilled byte is delivered");
        assert!(got == frame, "the spill is delivered in order");
    }

    fn wait_finished(join: &std::thread::JoinHandle<()>, what: &str) {
        let exited = aterm_spec::wait::within(MUST_HAPPEN, Duration::from_millis(1), || {
            join.is_finished().then_some(())
        });
        assert!(exited.is_some(), "{what} never exited");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_reply_writer_runs_at_the_class_the_keystroke_path_needs() {
        use std::os::unix::thread::JoinHandleExt as _;
        let (sink, mut peer) = sink_and_peer();
        let (reply_tx, join) =
            spawn_reply_writer(sink.clone(), ReportRoute::default()).expect("spawn");
        let thread = join.as_pthread_t() as libc::pthread_t;
        assert!(reply_tx.submit(Arc::from(&b"ok"[..])), "queued");
        let mut buf = [0u8; 2];
        peer.read_exact(&mut buf).expect("the reply is written");
        assert_eq!(&buf, b"ok");
        let observed = crate::qos::class_of(thread).expect("the writer's QoS class");
        assert_eq!(
            observed,
            crate::qos::class_for(crate::qos::Role::Interactive),
            "the reply writer runs at 0x{observed:x}, not the class the keystroke \
             path needs: it holds the sink lock the UI thread's own write spins on"
        );
        drop(reply_tx);
        drop(sink);
        wait_finished(&join, "the reply writer");
    }

    #[test]
    fn a_reattached_writer_ends_with_its_reader_and_the_first_with_the_sink() {
        let (sink, mut peer) = sink_and_peer();
        let (first_tx, first_join) =
            spawn_reply_writer(sink.clone(), ReportRoute::default()).expect("spawn");
        let (second_tx, second_join) =
            spawn_reply_writer(sink.clone(), ReportRoute::default()).expect("re-attach");
        drop(first_tx);
        drop(second_tx);
        wait_finished(&second_join, "the re-attached reply writer");
        spill_is_delivered(&sink, &mut peer);
        drop(sink);
        wait_finished(&first_join, "the first reply writer");
        let n = peer
            .read(&mut [0u8; 1])
            .expect("EOF, not a read timeout: the master fd must have closed");
        assert_eq!(n, 0, "EOF");
    }

    #[test]
    fn a_failed_spawn_leaves_the_arranger_slot_to_the_retried_attach() {
        let (sink, mut peer) = sink_and_peer();
        super::REPLY_WRITER_SPAWN_FAULT.with(|f| f.set(true));
        spawn_reply_writer(sink.clone(), ReportRoute::default())
            .expect_err("the injected spawn failure surfaces");
        assert!(
            !super::take_injected_reply_writer_spawn_fault(),
            "the fault is one-shot: consumed by the failed attempt"
        );
        let (retry_tx, retry_join) =
            spawn_reply_writer(sink.clone(), ReportRoute::default()).expect("the retry spawns");
        assert!(retry_tx.submit(Arc::from(&b"ok"[..])), "queued");
        let mut buf = [0u8; 2];
        peer.read_exact(&mut buf)
            .expect("the retry's writer writes");
        assert_eq!(&buf, b"ok");
        drop(retry_tx);
        spill_is_delivered(&sink, &mut peer);
        drop(sink);
        wait_finished(&retry_join, "the retried reply writer");
        let n = peer
            .read(&mut buf)
            .expect("EOF, not a read timeout: the master fd must have closed");
        assert_eq!(n, 0, "EOF");
    }
}

const COMPRESS_SIGNAL_CAP: usize = 1;
const COMPRESS_SIGNAL_AT: usize = 900;
const COMPRESS_BUDGET: usize = 256;
const COMPRESS_LOW_WATER: usize = 256;
const COMPRESS_QUIET_WINDOW: std::time::Duration = std::time::Duration::from_millis(50);
const COMPRESS_TRICKLE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

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
        std::thread::yield_now();
    }
}

#[cfg(test)]
mod flood_pressure_relief_tests {
    use super::relieve_flood_pressure;
    use aterm_core::scrollback::Scrollback;
    use aterm_core::terminal::Terminal;
    use std::sync::Mutex;
    use std::sync::atomic::AtomicU32;

    fn flooded(n: usize) -> Terminal {
        let mut t = Terminal::with_scrollback(24, 253, 8, Scrollback::new(64, 512, 8_000_000));
        t.set_compress_offload_active(true);
        let mut buf = Vec::with_capacity(n * 256);
        for i in 0..n {
            let row = format!("L{i} ");
            buf.extend_from_slice(row.as_bytes());
            buf.resize(buf.len() + 253 - row.len(), b'p');
            buf.extend_from_slice(b"\r\n");
        }
        t.process(&buf);
        t
    }

    #[test]
    fn relief_promotes_the_backlog_back_under_the_mark_and_loses_nothing() {
        let term = Mutex::new(flooded(15_000));
        let before = {
            let t = term.lock().unwrap();
            assert!(t.main_grid().flood_promotion_due() > 0, "precondition");
            t.lazy_backlog_len()
        };
        relieve_flood_pressure(&term, &AtomicU32::new(0));
        let t = term.lock().unwrap();
        assert_eq!(t.main_grid().flood_promotion_due(), 0, "under the mark");
        assert!(t.lazy_backlog_len() < before, "the backlog was promoted");
        assert_eq!(t.scrollback_truncated_lines(), 0, "nothing lost");
    }

    #[test]
    fn a_partial_pressure_batch_keeps_the_half_floor() {
        let term = Mutex::new(flooded(10_100));
        {
            let t = term.lock().unwrap();
            let due = t.main_grid().flood_promotion_due();
            assert!(due > 0 && due < super::COMPRESS_BUDGET, "a partial batch");
            assert_eq!(t.lazy_backlog_len() - due, 10_000, "the half floor");
        }
        relieve_flood_pressure(&term, &AtomicU32::new(0));
        let t = term.lock().unwrap();
        assert_eq!(t.lazy_backlog_len(), 10_000, "no batch overshoot");
        assert_eq!(t.scrollback_truncated_lines(), 0);
    }

    #[test]
    fn a_history_clear_and_refill_rechecks_the_pressure_floor() {
        let mut t = flooded(15_000);
        let owed = t.main_grid().flood_promotion_due();
        assert!(owed > super::COMPRESS_BUDGET, "several batches are owed");
        let before = t.lazy_backlog_len();
        assert_eq!(
            t.drain_lazy_pressure_bounded(super::COMPRESS_BUDGET),
            before - super::COMPRESS_BUDGET
        );
        let owed = owed - super::COMPRESS_BUDGET;

        t.process(b"\x1b[3J");
        assert_eq!(t.lazy_backlog_len(), 0, "ED3 erased the staged history");
        t.process(&b"after clear\r\n".repeat(128));
        let refilled = t.lazy_backlog_len();
        assert!(refilled > 0 && refilled < 10_000, "a new small backlog");
        assert_eq!(t.main_grid().flood_promotion_due(), 0);
        assert_eq!(
            t.drain_lazy_pressure_bounded(owed.min(super::COMPRESS_BUDGET)),
            refilled,
            "the old owed count cannot drain the new backlog below the floor"
        );
        assert_eq!(t.scrollback_truncated_lines(), 0);
    }

    #[test]
    fn relief_works_the_saved_primary_under_an_alt_screen() {
        let mut t = flooded(15_000);
        t.process(b"\x1b[?1049h");
        let before = t.lazy_backlog_len();
        assert!(t.main_grid().flood_promotion_due() > 0, "precondition");
        let term = Mutex::new(t);
        relieve_flood_pressure(&term, &AtomicU32::new(0));
        let t = term.lock().unwrap();
        assert_eq!(t.main_grid().flood_promotion_due(), 0);
        assert!(t.lazy_backlog_len() < before);
    }

    #[test]
    fn under_the_mark_relief_takes_nothing() {
        let term = Mutex::new(flooded(5_000));
        let before = term.lock().unwrap().lazy_backlog_len();
        assert!(before > 0, "precondition: a staged backlog");
        relieve_flood_pressure(&term, &AtomicU32::new(0));
        assert_eq!(term.lock().unwrap().lazy_backlog_len(), before);
    }

    #[test]
    fn a_log_flood_the_line_limit_holds_is_left_to_the_flood_gate() {
        const BUDGET: usize = 100_000_000;
        let mut t = Terminal::with_scrollback(24, 80, 8, Scrollback::new(64, 512, BUDGET));
        t.set_compress_offload_active(true);
        let mut buf = Vec::with_capacity(100_000 * 82);
        for i in 0..100_000 {
            let row = format!("L{i} ");
            buf.extend_from_slice(row.as_bytes());
            buf.resize(buf.len() + 80 - row.len(), b'p');
            buf.extend_from_slice(b"\r\n");
        }
        t.process(&buf);
        let before = t.lazy_backlog_len();
        assert!(
            before * 80 * 8 > BUDGET / 2,
            "precondition: {before} staged rows are past half the headroom"
        );
        let term = Mutex::new(t);
        relieve_flood_pressure(&term, &AtomicU32::new(0));
        let t = term.lock().unwrap();
        assert_eq!(t.lazy_backlog_len(), before, "relief took nothing");
        assert_eq!(t.scrollback_truncated_lines(), 0, "and nothing was cut");
    }
}

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

/// [`shell_exit_watch`] of a shell not yet reaped. A shell of this process's
/// own that no watch can be made for keeps a [`ShellExit`] with none: the
/// kernel refuses an exit watch on a process that has begun to exit (`ESRCH`)
/// a moment before that process can be collected, and a shell that fails at
/// once is there by the time a loaded machine attaches its reader (measured:
/// 290 of 300 such refusals came before the child was collectable). Its
/// parent can still wait for it ([`ShellExit::await_retired`]); without the
/// wait the exit was posted before its status could be read, and a shell
/// killed at start was read as a clean exit, its only pane closed
/// (`shell_exit_at_start_live_headless`, four suites beside eight CPU hogs,
/// load 357).
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

/// The most the gather waits, once its shell's PTY has closed or its exit fired, for the kernel to retire the shell before the exit is posted. A hang detector, never a latency budget (at 20 ms a starved shell that failed at start read as a clean exit); a stop ends the wait at once ([`ShellExit::await_retired`]).
#[cfg(unix)]
pub(crate) const SHELL_RETIRE_HANG: std::time::Duration = std::time::Duration::from_secs(60);

/// How often [`ShellExit::await_retired`] looks again for a shell that has
/// exited, or could not be watched, and is not collectable yet: a nap, never a
/// spin that would take the CPU the exiting process needs.
#[cfg(unix)]
const RETIRE_LOOK: std::time::Duration = std::time::Duration::from_millis(1);

/// How often [`ShellExit::await_retired`] of a gather with no wake pipe (fd
/// exhaustion) reads the gather's stop flag while the shell's exit watch is
/// silent: a stop is all such a gather has, and the wait is a minute long.
#[cfg(unix)]
const RETIRE_STOP_LOOK: std::time::Duration = std::time::Duration::from_millis(10);

/// A session's shell's exit watch ([`shell_exit_watch`]) and the pid it watches.
#[cfg(unix)]
pub(crate) struct ShellExit {
    /// The kernel's exit watch; `None` for a shell of this process's own that
    /// was already on its way out when its reader attached ([`shell_exit_of`]).
    watch: Option<std::os::fd::OwnedFd>,
    pid: u32,
}

/// Whether retirement was observed, remained unknown at the existing bound
/// or error, or was cancelled by the reader's stop/wake.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ShellRetirement {
    Retired,
    Unconfirmed,
    Cancelled,
}

#[cfg(unix)]
impl ShellRetirement {
    /// Timeout/error still use the existing exit policy; cancellation never
    /// authorizes the parser to post a shell exit.
    fn gather_message(self, ending: GatherMsg) -> GatherMsg {
        match self {
            Self::Cancelled => GatherMsg::Wake,
            Self::Retired | Self::Unconfirmed => ending,
        }
    }
}

#[cfg(unix)]
impl ShellExit {
    /// The watch's descriptor, `-1` (which `poll` ignores) without one.
    fn as_raw_fd(&self) -> i32 {
        self.watch
            .as_ref()
            .map_or(-1, std::os::fd::AsRawFd::as_raw_fd)
    }

    /// The gather's one wait before reporting the exit, bounded by
    /// [`SHELL_RETIRE_HANG`]. Cancellation must instead wake the parser.
    fn await_retired_at_exit(&self, wake_rd: i32, stop: &AtomicBool) -> ShellRetirement {
        self.await_retired(SHELL_RETIRE_HANG, wake_rd, stop)
    }

    /// Wait at most `within` for retirement without reaping. Stop/wake returns
    /// Cancelled, distinct from a collectable child or an unconfirmed timeout
    /// or error. With no wake pipe, the silent watch reads the stop flag every
    /// [`RETIRE_STOP_LOOK`].
    fn await_retired(
        &self,
        within: std::time::Duration,
        wake_rd: i32,
        stop: &AtomicBool,
    ) -> ShellRetirement {
        let deadline = Instant::now() + within;
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
                    // A negative descriptor is ignored by `poll`.
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
                // park_reader writes the wake before raising the stop flag.
                // Cancellation wins even if the shell watch became ready too.
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
            // SAFETY: `waitid` filled a `CLD_*` siginfo or, with nothing waitable under `WNOHANG`,
            // left it zeroed; `si_pid` is defined in both.
            let collectable = rc == 0 && unsafe { info.si_pid() } == pid;
            let finished = rc != 0 || collectable || Instant::now() >= deadline;
            // A negative descriptor is ignored by `poll`, which then only naps.
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

/// [`spawn_session`]'s holder adoption for a restored engine (card engine §3.8, lane A4).
#[cfg(unix)]
pub(crate) fn adopt_restored_holder(
    term: &Mutex<Terminal>,
    timeline: &Mutex<crate::session_timeline::SessionTimeline>,
    master: i32,
    root_pgid: i32,
    fg_probe: fn(i32) -> i32,
    edge_image: crate::holder_image::ReadImage,
) -> aterm_core::terminal::HolderAdoption {
    let cursor = {
        let timeline = timeline.lock().unwrap_or_else(|p| p.into_inner());
        let log = timeline.slog();
        log.opened_wall()
            .filter(|_| log.log().is_open(aterm_slog::Node::SESSION))
            .map(|since| (log.block_cursor(), Some(since)))
    };
    let (blocks, pgid) = {
        let term = term_lock(term);
        let blocks = cursor.map(|(from, since_wall)| {
            crate::session_status::block_read(
                &term,
                from,
                since_wall,
                crate::session_status::executing_detail(&term),
            )
        });
        (blocks, fg_probe(master))
    };
    let image = (pgid > 0).then(|| Arc::new(edge_image(pgid, None)));
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
    let adoption = term_lock(term).adopt_holder_after_restore(token, exact);
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

fn note_cut(t: &mut Terminal, cut: aterm_core::terminal::HolderCut) {
    if cut.to.is_some() || t.holder_fence().current_token().is_some() {
        t.note_holder_cut(cut);
        crate::metrics::note_holder_cut();
    }
}

/// Batch messages from the gather stage to the parse stage (unix pipeline).
#[cfg(unix)]
pub(crate) enum GatherMsg {
    /// The deferred start gate released.
    Started,
    /// Every byte the master held when this reader started, which a park left unread ([`UpdatePark`]), went out in the batches before this one.
    CaughtUp,
    /// One gathered batch (buffer, filled length, the foreground change it carries, the group whose bytes end it — `edge.to` with an edge — and, with an edge, `edge.to`'s image as the gather read it at the edge, `crate::holder_image`).
    Data(
        Vec<u8>,
        usize,
        Option<crate::foreground_handback::FgEdge>,
        i32,
        Option<Arc<crate::holder_image::HolderImage>>,
    ),
    /// A foreground edge with no byte in flight (card engine §3.3).
    Edge(
        crate::foreground_handback::FgEdge,
        i32,
        Option<Arc<crate::holder_image::HolderImage>>,
    ),
    /// The master EOF'd on its own (shell exited).
    Eof,
    /// The session's shell exited (its exit watch fired before the master's EOF), and what the master held then has been sent.
    Exited,
    /// Session teardown (wake pipe / stop flag).
    Wake,
    Reset(crate::manual_reset::ManualReset),
    /// A host path changed the engine with no output to follow (a resize, a config or cell-size change).
    Journal,
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
    lane_guard: crate::manual_reset::LaneGuard,
    journal: Option<Arc<crate::crash_restore::SessionJournal>>,
) -> Result<(), String> {
    crate::qos::spawn_lane_thread(
        format!("aterm-pty-gather-{id}"),
        crate::qos::Role::Responsive,
        move || {
            let _lane_guard = lane_guard;
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
            let mut exec =
                proc_watch.map(|watch| crate::exec_edges::ExecEdges::new(watch, edge_image, seed));
            let proc_rd = exec.as_ref().map_or(-1, crate::exec_edges::ExecEdges::fd);
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
                            // Distinct branch.
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
                // Stop before the next read, never between a read and its hand-off:
                // bytes drained from the kernel must reach the engine (the
                // checkpoint-tear contract), and the parse stage processes every
                // batch it was sent.
                if stop.load(std::sync::atomic::Ordering::Acquire) {
                    let _ = filled_tx.send(GatherMsg::Wake);
                    break;
                }
                let outcome = match after_exit {
                    None => aterm_pty::read_or_wake(master, &mut buf, wake_rd, exit_rd, proc_rd),
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
                            x.heard(&mut cutter, || fg_probe(master), 0);
                            if !send_byte_free(&mut cutter, x) {
                                break;
                            }
                        }
                    }
                    aterm_pty::ReadOutcome::Wake => {
                        let _ = filled_tx.send(GatherMsg::Wake);
                        break;
                    }
                    aterm_pty::ReadOutcome::Idle => {
                        // Wake-pipe-less fallback (fd exhaustion): honor the stop flag.
                        if stop.load(std::sync::atomic::Ordering::Acquire) {
                            let _ = filled_tx.send(GatherMsg::Wake);
                            break;
                        }
                    }
                    aterm_pty::ReadOutcome::Eof => {
                        // The shell closing its terminal on its way out: the
                        // exit it is about to make goes with this one.
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
                                    let absorbed = x.absorb(e.to);
                                    if absorbed {
                                        e.lay = filled;
                                    }
                                    x.read_edge(e.to, absorbed)
                                })
                            }),
                            (Some(e), None) => (e.to > 0).then(|| Arc::new(edge_image(e.to, None))),
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
                        if filled_tx
                            .send(GatherMsg::Data(buf, filled, edge, holder, image))
                            .is_err()
                        {
                            // Distinct branch.
                            break;
                        }
                        if owed > 0 {
                            owed = owed.saturating_sub(filled);
                            if owed == 0 {
                                let _ = filled_tx.send(GatherMsg::CaughtUp);
                            }
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
}

struct PtyReaderWiring {
    master: i32,
    id: u64,
    window: WindowId,
    term: Arc<Mutex<Terminal>>,
    post: WakePost,
    reply_tx: ReplyLane,
    compress_tx: Option<std::sync::mpsc::SyncSender<()>>,
    cast: Arc<std::sync::Mutex<crate::cast::CastRecorder>>,
    cast_epoch: Instant,
    cast_watermark: Arc<crate::cast::CastWatermark>,
    temporal_tx: Option<crate::temporal::TemporalTx>,
    spine_mark: crate::temporal::SpineMark,
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
    timeline: Arc<std::sync::Mutex<crate::session_timeline::SessionTimeline>>,
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

fn spawn_pty_reader(w: PtyReaderWiring) -> Result<std::thread::JoinHandle<()>, String> {
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
        timeline,
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
        let lane_generation = reset_lane.open(filled_tx.clone());
        let lane_guard = crate::manual_reset::LaneGuard {
            lane: reset_lane.clone(),
            generation: lane_generation,
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
    let parse = crate::qos::spawn_lane_thread(
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
            let mut spine_mark = spine_mark;
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
            let mut ingest = |buf: &[u8],
                              edge: Option<crate::foreground_handback::FgEdge>,
                              holder: i32,
                              image: Option<Arc<crate::holder_image::HolderImage>>,
                              read_at: Option<Instant>| {
                let arrived = (!buf.is_empty()).then(|| read_at.unwrap_or_else(Instant::now));
                if let Some(at) = arrived {
                    stamp_output_arrival(
                        &last_output_ns,
                        (at.saturating_duration_since(lat_epoch).as_nanos() as u64).max(1),
                    );
                    crate::echo_rtt::note_output_burst(id);
                }
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
                let spine_copy: Option<std::sync::Arc<[u8]>> =
                    temporal_tx.as_ref().map(|_| std::sync::Arc::from(buf));
                #[cfg(unix)]
                let mut journal_keyframe: Option<crate::crash_restore::Captured> = None;
                let (response, reports) = {
                    let bytes: &[u8] = spine_copy.as_deref().unwrap_or(buf);
                    if let Some(req) = pending_reset.take() {
                        let mut t = term_lock(&term);
                        cuts.observe(emitted, &t, &cast_watermark);
                        let spine_pre = temporal_tx
                            .as_ref()
                            .map(|_| crate::temporal::SpinePre::take(&t));
                        #[cfg(unix)]
                        let mut journal_pre = journal.as_ref().map(|j| j.pre(&t));
                        let (report, reset_bytes) = crate::manual_reset::apply(&mut t);
                        emitted += reset_bytes.len();
                        fg_owners.observe(holder, t.program_evidence(), t.take_evidence_asserted());
                        if !reset_bytes.is_empty()
                            && let (Some(tx), Some(pre)) = (&temporal_tx, spine_pre)
                        {
                            let input = crate::temporal::InputSlice::from(&reset_bytes[..]);
                            spine_mark.record(tx, pre, &t, input);
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
                    while off < bytes.len() || handback_at == Some(off) || lay_at == Some(off) {
                        let limit = match handback_at {
                            Some(at) if at > off => at,
                            _ => bytes.len(),
                        };
                        let limit = match lay_at {
                            Some(lay) if lay > off => limit.min(lay),
                            _ => limit,
                        };
                        let limit = arm
                            .as_ref()
                            .and_then(|a| a.next_cut(off))
                            .filter(|_| {
                                #[cfg(test)]
                                if crate::holder_image::hold_hook::no_c_cut(id) {
                                    return false;
                                }
                                true
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
                                Some(fg_owners.probe_edge(e.from, e.to, |pgid, role| {
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
                                match a.last_of(read.pgid) {
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
                                }
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
                        let (c_signed, shell_executing);
                        {
                            let waited = term_busy(&term).then(Instant::now);
                            let mut t = term_lock(&term);
                            crate::metrics::note_parse_hold(parse_hold_wait(waited));
                            t.holder_rings();
                            cuts.observe(emitted, &t, &cast_watermark);
                            let spine_pre = temporal_tx
                                .as_ref()
                                .map(|_| crate::temporal::SpinePre::take(&t));
                            #[cfg(unix)]
                            let mut journal_pre = journal.as_ref().map(|j| j.pre(&t));
                            if let (Some(e), Some(v)) = (edge, verdicts)
                                && e.from != e.to
                            {
                                handback_at = None;
                                fg_owners.observe(
                                    e.from,
                                    t.program_evidence(),
                                    t.take_evidence_asserted(),
                                );
                                let orphaned = fg_owners.orphaned_by(e.to, |p| v.gone(p)).is_some();
                                let run = if orphaned || v.gone(e.from) {
                                    t.foreground_handback_scoped(orphaned)
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
                                    if let (Some(tx), Some(pre)) = (&temporal_tx, spine_pre) {
                                        let input = crate::temporal::InputSlice::from(&h.bytes[..]);
                                        spine_mark.record(tx, pre, &t, input);
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
                                );
                            }
                            if let Some(cut) = arm_cut {
                                note_cut(&mut t, cut);
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
                            let started_before = t.started_command_seq();
                            if let Some(unread) = launch.process(&mut t, &bytes[off..end], &person)
                            {
                                launch_unread = Some(unread);
                            }
                            c_signed = t.started_command_seq() != started_before
                                && t.is_require_shell_integration_nonce();
                            shell_executing =
                                t.shell_state() == aterm_core::terminal::ShellState::Executing;
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
                            if let Some(r) = t.take_reports() {
                                match &mut reports {
                                    Some(a) => a.extend_from_slice(&r),
                                    None => reports = Some(r),
                                }
                            }
                            if let Some(r) = t.take_response() {
                                match &mut acc {
                                    Some(a) => a.extend_from_slice(&r),
                                    None => acc = Some(r),
                                }
                            }
                            backlog = t.lazy_backlog_len();
                            if let (Some(tx), Some(pre), Some(copy)) =
                                (&temporal_tx, spine_pre, &spine_copy)
                                && end > off
                            {
                                let input =
                                    crate::temporal::InputSlice::new(copy.clone(), off..end);
                                spine_mark.record(tx, pre, &t, input);
                            }
                            #[cfg(unix)]
                            if let (Some(j), Some(pre)) = (&journal, journal_pre.as_mut()) {
                                j.record(&t, pre, &bytes[off..end], false);
                                journal_keyframe = j.decide(&t, pre);
                            }
                        }
                        #[cfg(unix)]
                        if let (Some(j), Some(captured)) = (&journal, journal_keyframe.take()) {
                            j.write(captured);
                        }
                        if let Some((image, kind)) = offered {
                            publish_holder(image, kind, arm.as_mut(), &mut cadence);
                        }
                        if let Some(a) = arm.as_mut() {
                            a.note_shell(c_signed, shell_executing);
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
                if crate::holder_image::DEV_ROWS && card_probe_on && !buf.is_empty() {
                    let unseen = timeline
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .card_unseen();
                    if let Some(token) = unseen {
                        let left = match card_probe {
                            Some((t, left)) if t == token => left,
                            _ => crate::holder_image::CARD_PROBE_BATCHES,
                        };
                        if left > 0 {
                            card_probe = Some((token, left - 1));
                            let (rows, base): (Vec<String>, i64) = {
                                let t = term_lock(&term);
                                (
                                    (0..usize::from(t.rows()))
                                        .map(|r| crate::control::visible_row(&t, r))
                                        .collect(),
                                    i64::try_from(t.grid().base_y()).unwrap_or(i64::MAX),
                                )
                            };
                            let reading = crate::claude_card::read_card(id, &rows, None);
                            if let Some(card) = &reading.read {
                                let row =
                                    base.saturating_add(i64::try_from(card.place.top).unwrap_or(0));
                                timeline
                                    .lock()
                                    .unwrap_or_else(|p| p.into_inner())
                                    .note_card_seen(token, reading.rows_kind(), &card.version, row);
                            }
                        }
                    }
                }
                if let Some((req, report, _)) = &manual_run {
                    crate::manual_reset::record(id, &timeline, req.source, report);
                    if let Some(reply) = &req.reply {
                        let _ = reply.try_send(report.clone());
                    }
                }
                if backlog >= COMPRESS_SIGNAL_AT
                    && let Some(tx) = &compress_tx
                {
                    let _ = tx.try_send(());
                }
                if let Some(resp) = response {
                    let resp: std::sync::Arc<[u8]> = resp.into();
                    if let Some(tx) = &temporal_tx {
                        // The spine records the reply's instant, BEFORE handing it
                        // to the peer; its bytes stay out (a replay never re-emits
                        // a reply). Bounded `try_send` (see the RawIn tap above):
                        // dropped on a full queue or inside a break, and counted.
                        spine_mark.reply(tx);
                    }
                    let _ = reply_tx.submit(resp);
                }
                if let Some(report) = reports {
                    let report: std::sync::Arc<[u8]> = report.into();
                    if let Some(tx) = &temporal_tx {
                        spine_mark.reply(tx);
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
                // Stop is honored at the top of every iteration, never between a
                // read and its processing (bytes out of the kernel queue must reach
                // the engine). A flooding master keeps `poll` readable, so without
                // this the wake-pipe-less fallback could never park such a reader.
                if stop.load(std::sync::atomic::Ordering::Acquire) {
                    break;
                }
                let r = match aterm_pty::read_or_wake(master, &mut buf, wake_rd, -1, -1) {
                    aterm_pty::ReadOutcome::Wake => break,
                    aterm_pty::ReadOutcome::Idle => {
                        // Wake-pipe-less fallback (fd exhaustion): honor the fd-free
                        // stop flag `Session::drop` raises, so an orphan-pinned reader
                        // still exits. Else keep waiting.
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
                ingest(&buf[..r.0], None, 0, None, Some(r.1));
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
                        ingest(&[], Some(edge), holder, image, None);
                    }
                    Ok(GatherMsg::Data(batch, len, edge, holder, image)) => {
                        ingest(&batch[..len], edge, holder, image, None);
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
                        ingest(&[], None, 0, None, None);
                    }
                    Ok(GatherMsg::Journal) => {
                        if let Some(j) = &journal {
                            j.on_nudge(&term);
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
    // Load first: a flood between two presents finds the edge already stamped,
    // and a plain load leaves the line shared instead of a failing RMW.
    if last_output_ns.load(Ordering::Relaxed) == 0 {
        let _ = last_output_ns.compare_exchange(0, now_ns, Ordering::Relaxed, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod output_edge_stamp_tests {
    use super::stamp_output_arrival;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn first_unpresented_edge_wins_and_a_present_reopens_it() {
        let stamp = AtomicU64::new(0);
        stamp_output_arrival(&stamp, 10);
        stamp_output_arrival(&stamp, 20);
        stamp_output_arrival(&stamp, 30);
        assert_eq!(
            stamp.load(Ordering::Relaxed),
            10,
            "a multi-batch burst is measured from where the terminal first fell behind"
        );
        assert_eq!(stamp.swap(0, Ordering::Relaxed), 10);
        stamp_output_arrival(&stamp, 40);
        assert_eq!(
            stamp.load(Ordering::Relaxed),
            40,
            "the next edge arms freely once a present has booked the previous one"
        );
    }
}

/// The age at which a posted `Wake::Output` still waiting for its handler is booked late (`metrics` `output_wake_late`, a debug-level note), and the delay after which an idle event loop turns once more to look at an armed latch.
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

/// How long a burst of posts (each under [`OUTPUT_CADENCE_NS`] after the one before) must have run before its session counts as streaming.
pub(crate) const STREAM_AFTER_NS: u64 = 16_000_000;

/// How many posts a burst makes before its session counts as streaming even short of [`STREAM_AFTER_NS`].
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
    /// Nothing posted: the latch was armed (the chunk rides the wake already posted), another reader won the arm, or the send failed.
    Coalesced,
    /// A fresh wake on the latch's clear->armed edge.
    Posted,
    /// A stranded wake re-posted (see [`wake_stranded`]).
    Reposted,
    /// Nothing posted: the UI thread holds the latch ([`WAKE_LATCH_HELD`]) and the chunk left it dirty, so the hold's release runs the pass.
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
            // Distinct branch.
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

/// The latch word a `Wake::Output` pass stores before its work, from the `word` it found, so a chunk fed after the store posts again or leaves the hold dirty.
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
    /// Due and dirty: the caller runs the session's pass, which takes the hold again ([`output_latch_after_pass`]).
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

/// A read at least this big is fed as it is, and a held gather ([`gather_held_output`]) stops once it holds this much.
#[cfg(any(windows, test))]
pub(crate) const HELD_GATHER_ENOUGH: usize = 16 * 1024;

/// How long before its hold falls due a held gather is fed, so its bytes are in the engine when the hold's release runs the session's pass.
#[cfg(any(windows, test))]
pub(crate) const HELD_GATHER_MARGIN_NS: u64 = 500_000;

/// The shortest held gather.
#[cfg(any(windows, test))]
pub(crate) const HELD_GATHER_MIN_NS: u64 = 1_000_000;

/// How often a held gather looks at the pipe while it waits.
#[cfg(any(windows, test))]
pub(crate) const HELD_GATHER_STEP_NS: u64 = 1_000_000;

/// How far past its request a held gather's wait is taken to wake, so the gather asks that much less and ends on time.
#[cfg(any(windows, test))]
pub(crate) const HELD_GATHER_WAIT_SLACK_NS: u64 = 500_000;

/// Whether a reader that has just read `filled` bytes at `now_ns` gathers more before it feeds them ([`gather_held_output`]), and until when (`lat_epoch` ns).
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
    /// The wake was posted before `drained_through_ns`, a user-event drain ran after that instant, and the latch is still armed.
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
    drained_through_ns: u64,
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
    /// Judge every session's latch at `now_ns` (this `about_to_wait`'s stamp), heal the ones proven lost (cas from the observed word, so a concurrent handler clear or reader re-stamp is never clobbered), book the late ones once, and record `now_ns` as the next pass's drained-through instant.
    pub(crate) fn pass<'a>(
        &mut self,
        now_ns: u64,
        latches: impl IntoIterator<Item = (u64, &'a AtomicU64)>,
    ) -> OutputWakePass {
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
                             a user-event drain ran after it was posted and no handler cleared it)",
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
        self.drained_through_ns = now_ns;
        out
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
    }
}

/// The directory holding aterm's co-located CLI tools (`aterm-ctl`, `atpkg`) when running from a macOS .app bundle (`Contents/MacOS`), put on child sessions' path so the bundled Help's workflows run with no install step.
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

/// The two environment variables that make a no-terminal `sudo` reach aterm's askpass helper (`docs/DESIGN-sudo-askpass-2026-10-15.md` §3.1).
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

/// The one `("PATH", value)` pair the baseline `env_add` carries, composed because `aterm_pty::build_child_env` applies `env_add` by key overwrite (`docs/DESIGN-toolchain-reroute-2026-09-07.md` §"Reaching path").
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
    if let Some(bin) = bin_dir
        && !entries.contains(&bin)
    {
        entries.push(bin);
        injected = true;
    }
    if let Some(bundle) = bundle_dir
        && !entries.contains(&bundle)
    {
        entries.insert(0, bundle);
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

/// The managed `agents/` directory every session gets in front of its path, created at launch if missing (one `mkdir -p`; nothing blocks the first frame), so the first tab on a fresh machine already has it on path and finds the twins atpkg lays later.
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
    #[cfg(test)]
    LOGIN_SHELL_ASKS.with(|asks| asks.set(asks.get() + 1));
    let mut command = crate::qos::command(crate::qos::Role::Background, program);
    command
        .args(args)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .stderr(aterm_uds::spawnfd::Stdio::null());
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
    let deadline = std::time::Instant::now() + budget;
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
                #[cfg(unix)]
                if let Ok(group) = libc::pid_t::try_from(child.id()) {
                    unsafe { libc::killpg(group, libc::SIGKILL) };
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
        entries.retain(|e| e != agents);
        entries.insert(0, agents.to_string());
        if !entries.iter().any(|e| e == bin) {
            entries.push(bin.to_string());
        }
    }
    entries.join(&sep.to_string())
}

#[cfg(test)]
mod reroute_path_env_tests {
    use super::{bundle_dir, reroute_path_env};
    use std::path::Path;

    const SEP: char = if cfg!(windows) { ';' } else { ':' };
    const REROUTE: &str = "/Users//u/Library/Application Support/aterm/pkg/reroute";
    const BUNDLE: &str = "/Applications/aterm.app/Contents/MacOS";

    fn path(parts: &[&str]) -> String {
        parts.join(&SEP.to_string())
    }

    const AGENTS: &str = "/Users//u/Library/Application Support/aterm/pkg/agents";

    #[test]
    fn the_injected_dirs_lead_the_inherited_path_once_each() {
        type Case = (
            &'static str,
            [Option<&'static str>; 3],
            Option<String>,
            Option<String>,
        );
        let cases: [Case; 12] = [
            (
                "agents beside the reroute dir, moved out of the user's PATH",
                [Some(REROUTE), Some(AGENTS), Some(BUNDLE)],
                Some(path(&[
                    "/Users//u/.local/bin",
                    "/opt/homebrew/bin",
                    "/usr/bin",
                    AGENTS,
                ])),
                Some(path(&[
                    REROUTE,
                    AGENTS,
                    BUNDLE,
                    "/Users//u/.local/bin",
                    "/opt/homebrew/bin",
                    "/usr/bin",
                ])),
            ),
            (
                "the agents dir alone",
                [None, Some(AGENTS), None],
                Some("/usr/bin".to_string()),
                Some(path(&[AGENTS, "/usr/bin"])),
            ),
            (
                "reroute, bundle, inherited",
                [Some(REROUTE), None, Some(BUNDLE)],
                Some(path(&["/usr/bin", "/bin"])),
                Some(path(&[REROUTE, BUNDLE, "/usr/bin", "/bin"])),
            ),
            (
                "a later reroute dir moves to the front",
                [Some(REROUTE), None, None],
                Some(path(&[
                    "/opt/homebrew/bin",
                    "/Users//u/.cargo/bin",
                    REROUTE,
                    "/usr/bin",
                    REROUTE,
                ])),
                Some(path(&[
                    REROUTE,
                    "/opt/homebrew/bin",
                    "/Users//u/.cargo/bin",
                    "/usr/bin",
                ])),
            ),
            (
                "a reachable bundle dir is not stacked",
                [Some(REROUTE), None, Some(BUNDLE)],
                Some(path(&[BUNDLE, "/usr/bin"])),
                Some(path(&[REROUTE, BUNDLE, "/usr/bin"])),
            ),
            (
                "the bundle dir alone",
                [None, None, Some(BUNDLE)],
                Some("/usr/bin".to_string()),
                Some(path(&[BUNDLE, "/usr/bin"])),
            ),
            (
                "the bundle dir alone, already reachable",
                [None, None, Some(BUNDLE)],
                Some(path(&[BUNDLE, "/usr/bin"])),
                None,
            ),
            (
                "no inherited PATH",
                [Some(REROUTE), None, Some(BUNDLE)],
                None,
                Some(path(&[REROUTE, BUNDLE])),
            ),
            (
                "an empty inherited PATH",
                [Some(REROUTE), None, None],
                Some(String::new()),
                Some(REROUTE.to_string()),
            ),
            (
                "an empty entry survives",
                [Some(REROUTE), None, None],
                Some(path(&["/usr/bin", "", "/bin"])),
                Some(path(&[REROUTE, "/usr/bin", "", "/bin"])),
            ),
            (
                "nothing to inject",
                [None, None, None],
                Some("/usr/bin".to_string()),
                None,
            ),
            ("nothing at all", [None, None, None], None, None),
        ];
        for (what, [reroute, agents, bundle], inherited, want) in cases {
            let got = reroute_path_env(reroute, agents, bundle, inherited.as_deref(), None);
            assert!(
                got.as_ref().is_none_or(|(key, _)| key == "PATH"),
                "{what}: {got:?}"
            );
            let got = got.map(|(_, value)| value);
            assert_eq!(got, want, "{what}");
            if let Some(value) = got {
                assert!(
                    reroute_path_env(reroute, agents, bundle, Some(&value), None)
                        .is_none_or(|(_, again)| again == value),
                    "{what}: idempotent under nesting"
                );
            }
        }
    }

    #[test]
    fn the_agents_dir_is_ensured_at_launch_so_the_first_tab_is_covered() {
        use super::managed_agents_dir;
        let prefix = aterm_tempfile::tempdir().expect("scratch prefix");
        let layout = atpkg::store::Layout {
            prefix: prefix.path().join("pkg"),
        };
        assert!(
            !layout.agents_dir().is_dir(),
            "a fresh prefix has no agents/"
        );
        let dir = managed_agents_dir(&layout).expect("created, hence on PATH");
        assert_eq!(dir, layout.agents_dir().to_str().unwrap());
        assert!(layout.agents_dir().is_dir());
        assert_eq!(managed_agents_dir(&layout).as_deref(), Some(dir.as_str()));
        #[cfg(unix)]
        for (tag, plant) in [("link", true), ("file", false)] {
            let prefix = aterm_tempfile::tempdir().expect("scratch prefix");
            let layout = atpkg::store::Layout {
                prefix: prefix.path().join("pkg"),
            };
            std::fs::create_dir_all(&layout.prefix).expect("prefix");
            let elsewhere = prefix.path().join("elsewhere");
            std::fs::create_dir_all(&elsewhere).expect("target");
            if plant {
                std::os::unix::fs::symlink(&elsewhere, layout.agents_dir()).expect("link");
            } else {
                std::fs::write(layout.agents_dir(), b"x").expect("file");
            }
            assert_eq!(
                managed_agents_dir(&layout),
                None,
                "{tag} at agents/ is refused"
            );
            let md = std::fs::symlink_metadata(layout.agents_dir()).expect("left alone");
            assert_eq!(md.file_type().is_symlink(), plant, "{tag} left as it was");
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_atpkg_child_path_is_the_login_shells_or_the_fallback() {
        use super::{fallback_child_path, pick_login_path};
        let banner = "Welcome!\nLast login: never\n/Users//u/.local/bin:/Users//u/Library/Application Support/aterm/pkg/bin:/usr/bin\n";
        assert_eq!(
            pick_login_path(banner).as_deref(),
            Some("/Users//u/.local/bin:/Users//u/Library/Application Support/aterm/pkg/bin:/usr/bin")
        );
        assert_eq!(pick_login_path("nothing here\n"), None);
        assert_eq!(pick_login_path(""), None);
        let home = Path::new("/Users//u");
        let brew: &[&str] = if cfg!(target_os = "macos") {
            &["/opt/homebrew/bin"]
        } else {
            &[]
        };
        let with = |head: &[&str], tail: &[&str]| -> String {
            let mut all: Vec<&str> = head.to_vec();
            all.extend_from_slice(brew);
            all.extend_from_slice(tail);
            path(&all)
        };
        assert_eq!(
            fallback_child_path(Some("/usr/bin:/bin:/usr/sbin:/sbin"), Some(home), None),
            with(
                &[
                    "/usr/bin",
                    "/bin",
                    "/usr/sbin",
                    "/sbin",
                    "/Users//u/.local/bin"
                ],
                &["/usr/local/bin"]
            ),
            "launchd's four, then the foreign homes"
        );
        assert_eq!(
            fallback_child_path(Some("/usr/local/bin:/usr/bin"), None, None),
            with(&["/usr/local/bin", "/usr/bin"], &[]),
            "already-present dirs are not repeated; no home, no ~/.local/bin"
        );
        assert_eq!(
            fallback_child_path(None, Some(home), None),
            with(&["/Users//u/.local/bin"], &["/usr/local/bin"])
        );
        let agents = "/Users//u/Library/Application Support/aterm/pkg/agents";
        let bin = "/Users//u/Library/Application Support/aterm/pkg/bin";
        assert_eq!(
            fallback_child_path(
                Some(&format!("/usr/bin:{agents}:/bin")),
                Some(home),
                Some((agents, bin))
            ),
            with(
                &[agents, "/usr/bin", "/bin", "/Users//u/.local/bin"],
                &["/usr/local/bin", bin]
            ),
            "agents/ moved to the front, bin/ appended"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_path_probe_asks_the_accounts_shell_or_the_platforms_default() {
        use super::path_probe_shell;
        for shell in ["/bin/bash", "/usr/bin/zsh", "/usr/bin/fish"] {
            assert_eq!(path_probe_shell(shell, true), shell);
            assert_eq!(path_probe_shell(shell, false), shell);
        }
        for shell in ["/usr/bin/dash", "/bin/sh", "/bin/mksh"] {
            assert_eq!(
                path_probe_shell(shell, false),
                shell,
                "the sh family off macOS"
            );
            assert_eq!(path_probe_shell(shell, true), "/bin/zsh");
        }
        for shell in ["/bin/tcsh", "/usr/bin/nu", "", "xonsh"] {
            assert_eq!(path_probe_shell(shell, false), "/bin/sh", "{shell:?}");
            assert_eq!(path_probe_shell(shell, true), "/bin/zsh", "{shell:?}");
        }
    }

    #[test]
    fn a_first_run_update_takes_the_fallback_rather_than_wait_for_the_login_shell() {
        use super::update_path_with;
        let never = || -> &'static str { panic!("a first run waited for the login shell") };
        assert_eq!(
            update_path_with(true, None, never, || "/fallback".to_string()),
            "/fallback"
        );
        assert_eq!(
            update_path_with(false, None, || "/login", || unreachable!()),
            "/login",
            "a routine pass waits for the login shell"
        );
        for first_run in [true, false] {
            assert_eq!(
                update_path_with(first_run, Some("/login"), never, || unreachable!()),
                "/login",
                "the login shell's answer, once it is in"
            );
        }
    }

    #[test]
    #[cfg(windows)]
    fn login_shell_path_asks_no_shell_on_windows() {
        let asks = || super::LOGIN_SHELL_ASKS.with(std::cell::Cell::get);
        let git_bash = [
            r"C:\Program Files\Git\usr\bin\bash",
            r"C:\Program Files\Git\bin\bash",
        ]
        .into_iter()
        .find(|bash| std::path::Path::new(&format!("{bash}.exe")).is_file());
        let shell = git_bash.unwrap_or("/usr/bin/bash");

        let before = asks();
        let answered = super::login_shell_path_with(
            shell,
            &["-c", "printf '%s\\n' \"$PATH\""],
            std::time::Duration::from_secs(60),
        );
        assert_eq!(asks(), before + 1, "the control: a direct ask is counted");
        if let Some(bash) = git_bash {
            assert!(
                answered.is_some(),
                "the control: {bash} answers a PATH when asked directly"
            );
        }

        let before = asks();
        let path = crate::test_env::scoped("SHELL", shell, super::login_shell_path);
        assert_eq!(
            asks(),
            before,
            "SHELL={shell}: login_shell_path asked a shell"
        );
        assert_eq!(path, None, "SHELL={shell}");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn login_path_shell_runs_below_the_inherited_band() {
        let path = super::login_shell_path_with(
            "/bin/sh",
            &["-c", "printf '/pri:%s\\n' \"$(/bin/ps -o pri= -p $$)\""],
            aterm_spec::wait::MUST_HAPPEN,
        )
        .expect("the probe shell answered");
        let pri: i32 = path
            .trim_start_matches("/pri:")
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("no priority in {path:?}"));
        assert!(
            pri <= 20,
            "the login-shell probe ran at pri {pri}, not utility (20)"
        );
    }

    #[test]
    #[cfg(unix)]
    fn login_path_budget_bounds_a_grandchild_holding_stdout() {
        const JOB: std::time::Duration = std::time::Duration::from_secs(60);
        let script = format!("sleep {}; printf '/a:/b\\n'", JOB.as_secs());
        let started = std::time::Instant::now();
        let path = super::login_shell_path_with(
            "/bin/sh",
            &["-c", script.as_str()],
            std::time::Duration::from_millis(300),
        );
        let elapsed = started.elapsed();
        assert_eq!(path, None);
        assert!(elapsed < JOB, "waited for the grandchild: {elapsed:?}");
    }

    #[test]
    #[cfg(unix)]
    fn login_path_returns_despite_a_backgrounded_rc_job() {
        const JOB: std::time::Duration = std::time::Duration::from_secs(120);
        const BUDGET: std::time::Duration = aterm_spec::wait::MUST_HAPPEN;
        let script = format!("sleep {} & printf '/a:/b:%s\\n' \"$!\"", JOB.as_secs());
        let started = std::time::Instant::now();
        let path = super::login_shell_path_with("/bin/sh", &["-c", script.as_str()], BUDGET);
        let elapsed = started.elapsed();
        let job = path
            .as_deref()
            .and_then(|line| line.strip_prefix("/a:/b:"))
            .and_then(|pid| pid.parse::<libc::pid_t>().ok())
            .filter(|&pid| pid > 0);
        if let Some(pid) = job.filter(|_| elapsed < JOB) {
            // SAFETY: `kill` takes no pointers, and `pid` is positive, so it names one process and never a group.
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
        assert!(
            elapsed < JOB,
            "waited for the backgrounded job: {elapsed:?}"
        );
        assert!(job.is_some(), "no PATH carrying the job's pid: {path:?}");
    }

    #[test]
    #[cfg(unix)]
    fn login_path_answers_under_a_foreground_controlling_tty() {
        const CHILD: &str = "ATERM_TEST_LOGIN_PATH_UNDER_A_PTY";
        const SHELLS: [(&str, &[&str]); 2] = [
            ("/bin/zsh", &["-f", "-i", "-c", "printf '/a:/b\\n'"]),
            (
                "/bin/bash",
                &["--norc", "--noprofile", "-i", "-c", "printf '/a:/b\\n'"],
            ),
        ];
        let shells = || {
            SHELLS
                .into_iter()
                .filter(|(shell, _)| std::path::Path::new(shell).exists())
        };
        if std::env::var_os(CHILD).is_some() {
            // SAFETY: both calls take no pointers and only read process state.
            let (foreground, own) =
                unsafe { (libc::tcgetpgrp(libc::STDIN_FILENO), libc::getpgrp()) };
            assert_eq!(
                foreground, own,
                "the re-exec is not in the foreground of a controlling tty"
            );
            for (shell, args) in shells() {
                let started = std::time::Instant::now();
                let path = super::login_shell_path_with(shell, args, aterm_spec::wait::MUST_HAPPEN);
                let elapsed = started.elapsed();
                assert_eq!(path.as_deref(), Some("/a:/b"), "{shell} after {elapsed:?}");
            }
            return;
        }
        if shells().next().is_none() {
            aterm_spec::host::could_not_run(
                "no /bin/zsh or /bin/bash on this host; install zsh or bash",
            );
        }
        let (master, slave) = aterm_pty::open_pty_pair().expect("a pty pair");
        // SAFETY: two live descriptors this test owns, each taken by exactly
        // one `OwnedFd`; the stand-in's stdin is the copy std makes of `slave`.
        let (master, slave) = unsafe {
            use std::os::fd::{FromRawFd as _, OwnedFd};
            (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave))
        };
        let mut stand_in = crate::test_alone::StandIn::this_test();
        stand_in
            .command()
            .env(CHILD, "1")
            .env("HISTFILE", "/dev/null")
            .stdin(std::process::Stdio::from(slave));
        // SAFETY: the hook runs between fork and exec and calls only `setsid` and `ioctl`, both async-signal-safe, on the child's own stdin.
        unsafe {
            std::os::unix::process::CommandExt::pre_exec(stand_in.command(), || {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        stand_in.spawn().passed();
        drop(master);
    }

    #[test]
    fn the_bin_dir_is_appended_once_behind_the_inherited_path() {
        const BIN: &str = "/Users//u/Library/Application Support/aterm/pkg/bin";
        let inherited = path(&["/opt/homebrew/bin", "/usr/bin"]);
        let (_, value) = reroute_path_env(
            Some(REROUTE),
            Some(AGENTS),
            Some(BUNDLE),
            Some(&inherited),
            Some(BIN),
        )
        .expect("injects");
        assert_eq!(
            value,
            path(&[
                REROUTE,
                AGENTS,
                BUNDLE,
                "/opt/homebrew/bin",
                "/usr/bin",
                BIN
            ])
        );
        let (_, again) = reroute_path_env(
            Some(REROUTE),
            Some(AGENTS),
            Some(BUNDLE),
            Some(&value),
            Some(BIN),
        )
        .expect("injects");
        assert_eq!(
            again, value,
            "a PATH that holds it keeps it once, where it was"
        );
        let user_first = path(&[BIN, "/usr/bin"]);
        assert_eq!(
            reroute_path_env(None, None, None, Some(&user_first), Some(BIN)),
            None,
            "already there, wherever the user put it: nothing to inject"
        );
        assert_eq!(
            reroute_path_env(None, None, None, Some("/usr/bin"), Some(BIN)).map(|p| p.1),
            Some(path(&["/usr/bin", BIN]))
        );
        assert_eq!(
            reroute_path_env(None, None, None, None, Some(BIN)).map(|p| p.1),
            Some(BIN.to_string()),
            "no inherited PATH: no dangling separator"
        );
        assert_eq!(
            reroute_path_env(Some(REROUTE), Some(AGENTS), None, Some(&inherited), None)
                .map(|p| p.1),
            Some(path(&[REROUTE, AGENTS, "/opt/homebrew/bin", "/usr/bin"])),
            "None hands no bin/"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn bundle_dir_is_only_a_real_app_layout() {
        let app = bundle_dir(Some(Path::new(
            "/Applications/aterm.app/Contents/MacOS/aterm",
        )));
        if cfg!(target_os = "linux") {
            assert_eq!(
                app, None,
                "Linux has no app bundle: its rule is the store's"
            );
        } else {
            assert_eq!(app.as_deref(), Some(BUNDLE));
        }
        assert_eq!(
            bundle_dir(Some(Path::new("/Users//u/aterm/target/release/aterm-gui"))),
            None
        );
        assert_eq!(bundle_dir(None), None);
        let dir = aterm_tempfile::tempdir().expect("a store folder");
        let store = dir.path().canonicalize().expect("the store resolves");
        let exe = store.join("aterm");
        std::fs::write(&exe, b"").expect("the binary");
        assert_eq!(bundle_dir(Some(&exe)), None, "no aterm-ctl alias yet");
        std::os::unix::fs::symlink("aterm", store.join("aterm-ctl")).expect("the alias");
        assert_eq!(
            bundle_dir(Some(&exe)).as_deref(),
            cfg!(target_os = "linux").then(|| store.to_str().expect("a UTF-8 path")),
            "the store is the PATH folder on Linux only; macOS tabs take the bundle's"
        );
    }

    #[test]
    fn linux_store_dir_requires_aterm_ctl_sibling() {
        use super::linux_store_dir;
        use std::path::PathBuf;
        const STORE: &str = "/home/u/.local/lib/aterm/bin";
        let exe = Path::new(STORE).join("aterm");
        let ctl = Path::new(STORE).join("aterm-ctl");
        let installed = |path: &Path| (path == exe || path == ctl).then(|| exe.clone());
        assert_eq!(linux_store_dir(&exe, installed).as_deref(), Some(STORE));
        let alone = |path: &Path| (path == exe).then(|| exe.clone());
        assert_eq!(linux_store_dir(&exe, alone), None);
        let own = |path: &Path| Some(PathBuf::from(path));
        assert_eq!(
            linux_store_dir(Path::new("/home/u/work/target/debug/aterm"), own),
            None,
            "a build tree's aterm-ctl is a binary of its own"
        );
        assert_eq!(
            linux_store_dir(&Path::new(STORE).join("aterm-gui"), installed),
            None,
            "the links name `aterm`, so only a binary of that name is the store's"
        );
        let inherited = path(&["/usr/local/bin", "/usr/bin"]);
        assert_eq!(
            reroute_path_env(Some(REROUTE), None, Some(STORE), Some(&inherited), None).map(|p| p.1),
            Some(path(&[REROUTE, STORE, "/usr/local/bin", "/usr/bin"]))
        );
    }

    #[test]
    fn a_windows_app_folder_with_the_console_image_qualifies() {
        use super::windows_app_dir;
        const APP: &str = r"C:\Users\u\AppData\Local\Programs\aterm";
        let holds = |dir: &'static str| move |path: &Path| path == Path::new(dir).join("aterm.exe");
        for image in ["aterm-gui.exe", "aterm.exe", "aterm-windowed.exe"] {
            let exe = Path::new(APP).join(image);
            assert_eq!(
                windows_app_dir(&exe, holds(APP)).as_deref(),
                Some(APP),
                "{image}"
            );
        }
        assert_eq!(
            windows_app_dir(&Path::new(APP).join("aterm-gui.exe"), |_| false),
            None
        );
        assert_eq!(windows_app_dir(Path::new("aterm.exe"), |_| true), None);
        #[cfg(windows)]
        {
            assert_eq!(bundle_dir(None), None);
            let dir = aterm_tempfile::tempdir().expect("an app folder");
            let gui = dir.path().join("aterm-gui.exe");
            std::fs::write(&gui, b"").expect("the windowed image");
            assert_eq!(bundle_dir(Some(&gui)), None, "no console image yet");
            std::fs::write(dir.path().join("aterm.exe"), b"").expect("the console image");
            assert_eq!(bundle_dir(Some(&gui)).as_deref(), dir.path().to_str());
            let inherited = path(&[r"C:\Windows\system32", r"C:\Windows"]);
            assert_eq!(
                reroute_path_env(None, None, Some(APP), Some(&inherited), None).map(|p| p.1),
                Some(path(&[APP, r"C:\Windows\system32", r"C:\Windows"]))
            );
        }
    }
}

#[cfg(test)]
mod clipboard_coalesce_tests {
    use aterm_core::terminal::ClipboardSelection as Sel;
    use aterm_models_gui::clipboard_mailbox_model;
    use aterm_spec::derive::Model;

    use super::{
        ClipMailboxReceiver, ClipWrite, QuerySource, clip_mailbox, route_osc52_query,
        route_osc52_write,
    };

    fn set(clipboard: Option<&str>, primary: Option<&str>) -> ClipWrite {
        ClipWrite {
            clipboard: clipboard.map(From::from),
            primary: primary.map(From::from),
        }
    }

    fn assert_mailbox_projection(
        model: &Model,
        state: &aterm_spec::interp::State,
        receiver: &ClipMailboxReceiver,
        clipboard_generation: i64,
        primary_generation: i64,
    ) {
        let actual = receiver
            .mailbox
            .state
            .lock()
            .expect("clipboard mailbox lock");
        assert_eq!(state[&"pending"], i64::from(actual.pending.is_some()));
        assert_eq!(state[&"clipboard_generation"], clipboard_generation);
        assert_eq!(state[&"primary_generation"], primary_generation);
        let pending_clipboard_present = actual
            .pending
            .as_ref()
            .is_some_and(|pending| pending.clipboard.is_some());
        let pending_primary_present = actual
            .pending
            .as_ref()
            .is_some_and(|pending| pending.primary.is_some());
        assert_eq!(
            state[&"pending_clipboard_present"],
            i64::from(pending_clipboard_present)
        );
        assert_eq!(
            state[&"pending_primary_present"],
            i64::from(pending_primary_present)
        );
        if let Some(pending) = actual.pending.as_ref() {
            if let Some(generation) = pending.clipboard.as_deref() {
                assert_eq!(
                    state[&"pending_clipboard"],
                    generation
                        .parse::<i64>()
                        .expect("numeric clipboard generation")
                );
            }
            if let Some(generation) = pending.primary.as_deref() {
                assert_eq!(
                    state[&"pending_primary"],
                    generation
                        .parse::<i64>()
                        .expect("numeric PRIMARY generation")
                );
            }
        }
        assert!(model.check_invariant("OneLatestPendingWrite", state));
    }

    #[test]
    fn osc52_writes_and_reads_the_slot_its_target_names() {
        type Slots<'a> = (Option<&'a str>, Option<&'a str>);
        let cases: [(&[Sel], bool, Slots, QuerySource); 6] = [
            (
                &[Sel::Clipboard],
                true,
                (Some("x"), None),
                QuerySource::Clipboard,
            ),
            (&[], true, (Some("x"), None), QuerySource::Clipboard),
            (
                &[Sel::Clipboard, Sel::Primary],
                true,
                (Some("x"), Some("x")),
                QuerySource::Clipboard,
            ),
            (
                &[Sel::Primary],
                true,
                (None, Some("x")),
                QuerySource::Primary,
            ),
            (
                &[Sel::Primary],
                false,
                (Some("x"), None),
                QuerySource::Clipboard,
            ),
            (
                &[Sel::Clipboard, Sel::Primary],
                false,
                (Some("x"), None),
                QuerySource::Clipboard,
            ),
        ];
        for (sels, x11, slots, source) in cases {
            let w = route_osc52_write(sels, "x", x11);
            assert_eq!(
                (w.clipboard.as_deref(), w.primary.as_deref()),
                slots,
                "{sels:?} x11={x11}"
            );
            assert_eq!(route_osc52_query(sels, x11), source, "{sels:?} x11={x11}");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn query_arm_answers_primary_text_for_p_and_clipboard_text_for_c() {
        crate::control::PBPASTE_STUB.with(|s| *s.borrow_mut() = Some("clip".to_string()));
        crate::control::PRIMARY_STUB.with(|s| *s.borrow_mut() = Some("prim".to_string()));
        let read = |sels: &[Sel]| match route_osc52_query(sels, true) {
            QuerySource::Primary => crate::control::primary_get_owned(),
            QuerySource::Clipboard => crate::control::pbpaste_owned(),
        };
        assert_eq!(read(&[Sel::Primary]).as_deref(), Some("prim"));
        assert_eq!(read(&[Sel::Clipboard]).as_deref(), Some("clip"));
        assert_eq!(read(&[]).as_deref(), Some("clip"));
        crate::control::PBPASTE_STUB.with(|s| *s.borrow_mut() = None);
        crate::control::PRIMARY_STUB.with(|s| *s.borrow_mut() = None);
    }

    #[test]
    fn mailbox_drains_final_write_then_closes() {
        let (tx, rx) = clip_mailbox();
        tx.publish(set(Some("only"), None));
        drop(tx);
        let folded = rx.recv().expect("retained final write");
        assert_eq!(
            (folded.clipboard.as_deref(), folded.primary),
            (Some("only"), None)
        );
        assert!(rx.recv().is_none());
    }

    #[test]
    fn real_mailbox_conforms_to_the_derived_latest_slot_model() {
        let model = clipboard_mailbox_model();
        let mut state = model.init_state();
        let (tx, rx) = clip_mailbox();

        assert!(model.action_enabled("PublishClipboard", &state));
        tx.publish(set(Some("1"), None));
        assert!(model.fire("PublishClipboard", &mut state));
        assert_mailbox_projection(&model, &state, &rx, 1, 0);

        assert!(model.action_enabled("PublishPrimary", &state));
        tx.publish(set(None, Some("1")));
        assert!(model.fire("PublishPrimary", &mut state));
        assert_mailbox_projection(&model, &state, &rx, 1, 1);

        assert!(model.action_enabled("PublishClipboard", &state));
        tx.publish(set(Some("2"), None));
        assert!(model.fire("PublishClipboard", &mut state));
        assert_mailbox_projection(&model, &state, &rx, 2, 1);

        assert!(model.action_enabled("Consume", &state));
        let delivered = rx.recv().expect("one coalesced write");
        assert_eq!(delivered.clipboard.as_deref(), Some("2"));
        assert_eq!(delivered.primary.as_deref(), Some("1"));
        assert!(model.fire("Consume", &mut state));
        assert_mailbox_projection(&model, &state, &rx, 2, 1);

        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let mut retired = buggy.init_state();
        assert!(buggy.fire("PublishClipboard", &mut retired));
        assert!(buggy.check_invariant("OneLatestPendingWrite", &retired));
        assert!(buggy.fire("PublishClipboard", &mut retired));
        assert!(!buggy.check_invariant("OneLatestPendingWrite", &retired));
    }
}

#[cfg(test)]
mod host_reach_tests {
    use std::sync::mpsc::{Receiver, RecvTimeoutError, channel, sync_channel};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use aterm_containment::ContainmentMode as Cm;
    use aterm_core::config::TerminalConfig;
    use aterm_core::terminal::Terminal;

    use super::{
        ClipWrite, configure_notifications, host_reach, install_clipboard, new_live_terminal,
        term_lock,
    };

    const HANG: Duration = Duration::from_secs(60);

    const SET: &[u8] = b"\x1b]52;c;cGxhbnRlZA==\x07";

    fn query_allowed() -> TerminalConfig {
        TerminalConfig {
            allow_osc52_query: true,
            ..TerminalConfig::default()
        }
    }

    fn session(mode: Cm) -> (Arc<Mutex<Terminal>>, Receiver<ClipWrite>) {
        let term = Arc::new(Mutex::new(new_live_terminal(
            24,
            80,
            Some(&query_allowed()),
            None,
        )));
        let (tx, rx) = channel();
        install_clipboard(&term, host_reach(mode), move |write| {
            let _ = tx.send(write);
        });
        term_lock(&term).apply_policy_engine(aterm_policy::engine::PolicyEngine::new(
            aterm_policy::profiles::standard(),
        ));
        (term, rx)
    }

    fn query_reply(term: &Arc<Mutex<Terminal>>) -> Option<Vec<u8>> {
        crate::control::PBPASTE_STUB.with(|b| *b.borrow_mut() = Some("owner-secret".into()));
        crate::host_io_prefetch::feed_as_reader(term, b"\x1b]52;c;?\x07");
        let reply = term_lock(term).take_response();
        crate::control::PBPASTE_STUB.with(|b| *b.borrow_mut() = None);
        reply
    }

    #[test]
    #[cfg(not(target_os = "linux"))]
    fn an_osc52_query_reads_the_pasteboard_before_the_terminal_lock() {
        let (term, _writes) = session(Cm::User);
        crate::control::PBPASTE_STUB.with(|b| *b.borrow_mut() = Some("owner-secret".into()));
        term_lock(&term).process(b"\x1b]52;c;?\x07");
        let in_the_hold = term_lock(&term).take_response();
        crate::control::PBPASTE_STUB.with(|b| *b.borrow_mut() = None);
        assert_eq!(
            in_the_hold, None,
            "the pasteboard was read under the terminal lock"
        );
        crate::control::PBPASTE_STUB.with(|b| *b.borrow_mut() = Some("owner-secret".into()));
        crate::host_io_prefetch::feed_as_reader(&term, b"\x1b]52;c;?\x07\x1b[c");
        let reply = term_lock(&term).take_response().unwrap_or_default();
        crate::control::PBPASTE_STUB.with(|b| *b.borrow_mut() = None);
        assert!(
            reply.starts_with(b"\x1b]52;c;b3duZXItc2VjcmV0\x07\x1b[?"),
            "{:?}",
            String::from_utf8_lossy(&reply)
        );
    }

    #[test]
    fn the_clipboard_worker_starts_with_the_first_write() {
        crate::work_counts::take_clipboard_workers_started();
        let (term, writes) = session(Cm::User);
        assert_eq!(
            crate::work_counts::take_clipboard_workers_started(),
            0,
            "no worker for a session that wrote nothing"
        );
        term_lock(&term).process(SET);
        let first = writes.recv_timeout(HANG).expect("the first write arrives");
        assert_eq!(first.clipboard.as_deref(), Some("planted"));
        term_lock(&term).process(SET);
        writes.recv_timeout(HANG).expect("the second write arrives");
        assert_eq!(
            crate::work_counts::take_clipboard_workers_started(),
            1,
            "one worker, started by the first write"
        );
    }

    #[test]
    fn a_contained_session_reaches_no_clipboard() {
        let answer = b"\x1b]52;c;b3duZXItc2VjcmV0\x07".to_vec();
        for mode in [Cm::User, Cm::Safety] {
            let (term, writes) = session(mode);
            term_lock(&term).process(SET);
            let write = writes
                .recv_timeout(HANG)
                .unwrap_or_else(|e| panic!("{mode}: the OSC 52 write never arrived ({e:?})"));
            assert_eq!(write.clipboard.as_deref(), Some("planted"), "{mode}");
            assert_eq!(query_reply(&term), Some(answer.clone()), "{mode}");
        }

        let (term, writes) = session(Cm::Containment);
        term_lock(&term).process(SET);
        match writes.recv_timeout(HANG) {
            Err(RecvTimeoutError::Disconnected) => {}
            Ok(write) => panic!(
                "a contained OSC 52 write reached the clipboard: {:?}",
                write.clipboard
            ),
            Err(RecvTimeoutError::Timeout) => {
                panic!("the contained session kept a clipboard writer")
            }
        }
        assert!(
            !term_lock(&term).modes().allow_osc52_set,
            "a contained session holds no clipboard write authority"
        );
        assert_eq!(query_reply(&term), None, "a contained query is answered");
        term_lock(&term).apply_config(&query_allowed());
        assert_eq!(
            query_reply(&term),
            None,
            "a contained query is answered after a reload"
        );
    }

    fn notifications_allowed() -> TerminalConfig {
        TerminalConfig {
            allow_notifications: true,
            ..TerminalConfig::default()
        }
    }

    const NOTICES: [&[u8]; 3] = [
        b"\x1b]9;from the tab\x07",
        b"\x1b]99;;from the tab\x1b\\",
        b"\x1b]777;notify;title;from the tab\x07",
    ];

    fn posted(term: &Arc<Mutex<Terminal>>, notices: &Receiver<super::notify::NotifyMsg>) -> usize {
        for notice in NOTICES {
            term_lock(term).process(notice);
        }
        notices.try_iter().count()
    }

    #[test]
    fn a_contained_session_posts_no_notification() {
        for mode in [Cm::User, Cm::Safety, Cm::Containment] {
            let term = Arc::new(Mutex::new(new_live_terminal(
                24,
                80,
                Some(&notifications_allowed()),
                None,
            )));
            let (tx, notices) = sync_channel(8);
            configure_notifications(&term, host_reach(mode), &tx, 1, &Arc::default());
            term_lock(&term).apply_policy_engine(aterm_policy::engine::PolicyEngine::new(
                aterm_policy::profiles::standard(),
            ));
            if mode == Cm::Containment {
                assert_eq!(
                    posted(&term, &notices),
                    0,
                    "a contained program posted a desktop notification"
                );
                term_lock(&term).apply_config(&notifications_allowed());
                assert_eq!(
                    posted(&term, &notices),
                    0,
                    "a contained program posted a desktop notification after a reload"
                );
            } else {
                assert_eq!(posted(&term, &notices), NOTICES.len(), "{mode}");
            }
        }
    }
}

#[cfg(test)]
mod publish_burst_tests {
    use std::sync::Arc;

    use super::publish_burst;
    use crate::cast::ByteFanout;

    fn queued_at_wake(publish: impl FnOnce(&ByteFanout, &Arc<[u8]>, &dyn Fn())) -> usize {
        let fanout = Arc::new(ByteFanout::new());
        let sub = fanout.subscribe();
        let seen = std::cell::Cell::new(None);
        let burst: Arc<[u8]> = Arc::from(&b"out"[..]);
        publish(&fanout, &burst, &|| seen.set(Some(sub.drain().0.len())));
        seen.get().expect("the wake fired")
    }

    #[test]
    fn a_woken_bytes_subscriber_finds_the_burst() {
        assert_eq!(
            queued_at_wake(|fanout, burst, wake| publish_burst(fanout, burst, 0, wake)),
            1
        );
        let wake_then_tee = |fanout: &ByteFanout, burst: &Arc<[u8]>, wake: &dyn Fn()| {
            wake();
            fanout.tee(burst, 0);
        };
        assert_eq!(queued_at_wake(wake_then_tee), 0, "the negative control");
    }

    #[test]
    fn a_burst_carries_its_arrival_on_the_frame_clock() {
        let fanout = Arc::new(ByteFanout::new());
        let sub = fanout.subscribe();
        let _ = crate::metrics::now_us();
        let at = std::time::Instant::now();
        let burst: Arc<[u8]> = Arc::from(&b"out"[..]);
        publish_burst(&fanout, &burst, crate::metrics::us_at(at), || {});
        let (bursts, _) = sub.drain();
        let back = crate::metrics::instant_at_us(bursts[0].at_us).expect("in range");
        assert!(
            back <= at && at - back < std::time::Duration::from_micros(1),
            "the stamp is the arrival, floored to the microsecond"
        );
    }
}

#[cfg(test)]
mod held_gather_tests {
    use super::{
        HELD_GATHER_ENOUGH, HELD_GATHER_MARGIN_NS, HELD_GATHER_MIN_NS, HELD_GATHER_STEP_NS,
        HELD_GATHER_WAIT_SLACK_NS, HoldRelease, OUTPUT_CADENCE_NS, PostCadence, STREAM_AFTER_NS,
        WAKE_LATCH_DIRTY, WAKE_LATCH_HELD, WAKE_LATCH_SENT, WAKE_LATCH_STREAMING, WakeGate,
        begin_held_gather, gated_output_wake, gather_held_output, output_latch_after_pass,
        release_output_hold,
    };
    use std::cell::{Cell, RefCell};
    use std::sync::atomic::{AtomicU64, Ordering};

    const MS: u64 = 1_000_000;
    const US: u64 = 1_000;

    #[test]
    fn a_gather_begins_only_on_a_held_latch_and_ends_before_its_due() {
        let now = 100 * MS;
        for word in [
            0,
            now,
            now | WAKE_LATCH_SENT,
            now | WAKE_LATCH_SENT | WAKE_LATCH_STREAMING,
        ] {
            let latch = AtomicU64::new(word);
            assert_eq!(begin_held_gather(&latch, now, 100, false), None);
            assert_eq!(latch.load(Ordering::Relaxed), word, "untouched");
        }
        let due = now + OUTPUT_CADENCE_NS;
        let held = WAKE_LATCH_HELD | due;
        for (filled, typing) in [(100, true), (HELD_GATHER_ENOUGH, false)] {
            let latch = AtomicU64::new(held);
            assert_eq!(begin_held_gather(&latch, now, filled, typing), None);
            assert_eq!(latch.load(Ordering::Relaxed), held, "untouched");
        }
        let latch = AtomicU64::new(held);
        assert_eq!(
            begin_held_gather(&latch, now, 100, false),
            Some(due - HELD_GATHER_MARGIN_NS)
        );
        assert_eq!(latch.load(Ordering::Relaxed), held | WAKE_LATCH_DIRTY);
        assert_eq!(
            begin_held_gather(&latch, now, 100, false),
            Some(due - HELD_GATHER_MARGIN_NS)
        );
        assert_eq!(latch.load(Ordering::Relaxed), held | WAKE_LATCH_DIRTY);
        for at in [due - HELD_GATHER_MARGIN_NS, due, due + 7 * MS] {
            let latch = AtomicU64::new(held);
            assert_eq!(
                begin_held_gather(&latch, at, 100, false),
                Some(at + HELD_GATHER_MIN_NS)
            );
        }
        let far = AtomicU64::new(WAKE_LATCH_HELD | (now + 50 * MS));
        assert_eq!(
            begin_held_gather(&far, now, 100, false),
            Some(now + OUTPUT_CADENCE_NS)
        );
    }

    struct Pipe {
        first: u64,
        gap: u64,
        len: usize,
        lines: usize,
        taken: Cell<usize>,
        clock: Cell<u64>,
    }

    impl Pipe {
        fn top_up(&self, buf: &mut [u8], filled: usize) -> usize {
            let written = if self.clock.get() < self.first {
                0
            } else {
                (((self.clock.get() - self.first) / self.gap) as usize + 1).min(self.lines)
            };
            let fit = (buf.len() - filled) / self.len;
            let n = written.saturating_sub(self.taken.get()).min(fit);
            self.taken.set(self.taken.get() + n);
            filled + n * self.len
        }
    }

    #[test]
    fn a_gather_collects_until_its_deadline_a_key_or_a_batch() {
        let pipe = Pipe {
            first: 0,
            gap: 100 * US,
            len: 100,
            lines: 1_000,
            taken: Cell::new(1),
            clock: Cell::new(0),
        };
        let mut buf = vec![0u8; 65_536];
        let until = 3_500 * US;
        for overshoot in [HELD_GATHER_WAIT_SLACK_NS, 0] {
            pipe.taken.set(1);
            pipe.clock.set(0);
            let waits = RefCell::new(Vec::new());
            let filled = gather_held_output(
                &mut buf,
                100,
                until,
                || pipe.clock.get(),
                |b, f| pipe.top_up(b, f),
                |d| {
                    waits.borrow_mut().push(d);
                    pipe.clock
                        .set(pipe.clock.get() + d.as_nanos() as u64 + overshoot);
                },
                || true,
            );
            let end = pipe.clock.get();
            assert!(
                end <= until && until - end <= HELD_GATHER_WAIT_SLACK_NS,
                "overshoot {overshoot}: it ends on its deadline, not after (at {end})"
            );
            let written = (end / pipe.gap + 1) as usize;
            assert_eq!(
                filled,
                written * 100,
                "each line written by its end, in one batch"
            );
            assert!(
                waits
                    .borrow()
                    .iter()
                    .all(|d| d.as_nanos() as u64 <= HELD_GATHER_STEP_NS),
                "{waits:?}"
            );
        }
        let pipe = Pipe {
            taken: Cell::new(1),
            clock: Cell::new(0),
            ..pipe
        };
        let steps = Cell::new(0);
        let filled = gather_held_output(
            &mut buf,
            100,
            until,
            || pipe.clock.get(),
            |b, f| pipe.top_up(b, f),
            |d| pipe.clock.set(pipe.clock.get() + d.as_nanos() as u64),
            || {
                steps.set(steps.get() + 1);
                steps.get() < 2
            },
        );
        assert_eq!(
            pipe.clock.get(),
            HELD_GATHER_STEP_NS,
            "one step, then the key"
        );
        assert_eq!(filled, 11 * 100);
        let fast = Pipe {
            first: 0,
            gap: US,
            len: 100,
            lines: 1_000,
            taken: Cell::new(1),
            clock: Cell::new(1_000 * US),
        };
        let filled = gather_held_output(
            &mut buf,
            100,
            until,
            || fast.clock.get(),
            |b, f| fast.top_up(b, f),
            |_| panic!("no wait: the pipe already held a batch"),
            || true,
        );
        assert!(filled >= HELD_GATHER_ENOUGH, "{filled}");
        assert_eq!(fast.clock.get(), 1_000 * US);
    }

    struct Ui {
        latch: AtomicU64,
        queued: Cell<Option<u64>>,
        deadline: Cell<Option<u64>>,
        late: u64,
        passes: RefCell<Vec<u64>>,
    }

    impl Ui {
        fn pass(&self, now: u64) {
            let word = self.latch.load(Ordering::Relaxed);
            self.latch
                .store(output_latch_after_pass(word, now), Ordering::Relaxed);
            self.passes.borrow_mut().push(now);
        }

        fn turns_until(&self, t: u64) {
            loop {
                let release_at = self.deadline.get().map(|d| d + self.late);
                let wake_turn = self
                    .queued
                    .get()
                    .filter(|&q| release_at.is_none_or(|d| q <= d));
                let Some(now) = wake_turn.or(release_at).filter(|&at| at <= t) else {
                    return;
                };
                if wake_turn.is_some() {
                    self.queued.set(None);
                    self.pass(now);
                }
                self.deadline
                    .set(match release_output_hold(&self.latch, now) {
                        HoldRelease::Pending { due_ns } => Some(due_ns),
                        HoldRelease::Due => {
                            self.pass(now);
                            Some(now + OUTPUT_CADENCE_NS)
                        }
                        HoldRelease::NotHeld | HoldRelease::Released => None,
                    });
            }
        }
    }

    #[derive(Clone, Copy, Debug)]
    struct Timing {
        feed: u64,
        overshoot: u64,
        late: u64,
    }

    fn drive_reader(pipe: &Pipe, gather: bool, timing: Timing) -> (Vec<u64>, usize, Vec<u64>) {
        let ui = Ui {
            latch: AtomicU64::new(0),
            queued: Cell::new(None),
            deadline: Cell::new(None),
            late: timing.late,
            passes: RefCell::new(Vec::new()),
        };
        let mut cadence = PostCadence::default();
        let mut fed_at = Vec::new();
        let mut feeds = 0;
        let mut buf = vec![0u8; 65_536];
        while pipe.taken.get() < pipe.lines {
            let next = pipe.first + pipe.taken.get() as u64 * pipe.gap;
            pipe.clock.set(pipe.clock.get().max(next));
            ui.turns_until(pipe.clock.get());
            let before = pipe.taken.get();
            let mut filled = pipe.top_up(&mut buf, 0);
            if gather
                && let Some(until) = begin_held_gather(&ui.latch, pipe.clock.get(), filled, false)
            {
                filled = gather_held_output(
                    &mut buf,
                    filled,
                    until,
                    || pipe.clock.get(),
                    |b, f| pipe.top_up(b, f),
                    |d| {
                        pipe.clock
                            .set(pipe.clock.get() + d.as_nanos() as u64 + timing.overshoot);
                        ui.turns_until(pipe.clock.get());
                    },
                    || true,
                );
            }
            assert_eq!(filled, (pipe.taken.get() - before) * pipe.len);
            ui.turns_until(pipe.clock.get());
            let now = pipe.clock.get();
            fed_at.extend(std::iter::repeat_n(now, pipe.taken.get() - before));
            feeds += 1;
            let gate =
                gated_output_wake(&ui.latch, now, 0, cadence.streaming(now), || true, || now);
            match gate {
                WakeGate::Posted | WakeGate::Reposted => {
                    cadence.posted(now);
                    ui.queued.set(Some(now + 20 * US));
                }
                WakeGate::Held => cadence.held(now),
                WakeGate::Coalesced => {}
            }
            pipe.clock.set(now + timing.feed);
        }
        ui.turns_until(u64::MAX);
        assert_eq!(ui.latch.load(Ordering::Relaxed), 0, "quiet: nothing held");
        (fed_at, feeds, ui.passes.into_inner())
    }

    #[test]
    fn a_held_stream_is_fed_once_per_hold_not_once_per_line() {
        let pipe = || Pipe {
            first: 1_000 * MS,
            gap: 100 * US,
            len: 100,
            lines: 2_000,
            taken: Cell::new(0),
            clock: Cell::new(0),
        };
        for (overshoot, late) in [
            (0, 0),
            (HELD_GATHER_WAIT_SLACK_NS, 0),
            (HELD_GATHER_WAIT_SLACK_NS, 500 * US),
            (0, MS),
        ] {
            let timing = Timing {
                feed: 20 * US,
                overshoot,
                late,
            };
            let p = pipe();
            let (fed_at, feeds, passes) = drive_reader(&p, true, timing);
            let bound = (STREAM_AFTER_NS / (100 * US)
                + 4 * (200 * MS - STREAM_AFTER_NS) / OUTPUT_CADENCE_NS
                + 8) as usize;
            assert!(
                feeds <= bound,
                "{timing:?}: {feeds} feeds for 2,000 lines (at most {bound})"
            );
            let lag = OUTPUT_CADENCE_NS + HELD_GATHER_MARGIN_NS + HELD_GATHER_MIN_NS + late;
            for (i, &fed) in fed_at.iter().enumerate() {
                let written = p.first + i as u64 * p.gap;
                let pass = passes.iter().find(|&&at| at > fed).copied();
                assert!(
                    pass.is_some_and(|at| at - written <= lag),
                    "{timing:?}: line {i}, written at {} µs and fed at {} µs, waits for {pass:?}",
                    written / US,
                    fed / US
                );
            }
            let (_, feeds_ungathered, _) = drive_reader(&pipe(), false, timing);
            assert_eq!(feeds_ungathered, 2_000, "{timing:?}");
        }
    }

    #[test]
    fn a_typed_echo_is_never_gathered() {
        for key in 0..40u64 {
            let p = Pipe {
                first: 1_000 * MS + key * 80 * MS,
                gap: 40 * US,
                len: 12,
                lines: 7,
                taken: Cell::new(0),
                clock: Cell::new(0),
            };
            let timing = Timing {
                feed: 20 * US,
                overshoot: HELD_GATHER_WAIT_SLACK_NS,
                late: 0,
            };
            let (fed_at, feeds, _) = drive_reader(&p, true, timing);
            assert_eq!(feeds, 7, "key {key}: a feed per read");
            for (i, &fed) in fed_at.iter().enumerate() {
                assert_eq!(fed, p.first + i as u64 * p.gap, "key {key}: fed as read");
            }
        }
    }
}

#[cfg(test)]
mod output_wake_coalesce_tests {
    use super::{
        HoldRelease, LoopIdle, OUTPUT_CADENCE_NS, OutputWakeVerdict, OutputWakeWatch, PostCadence,
        STREAM_AFTER_NS, STREAM_AFTER_POSTS, WAKE_LATCH_DIRTY, WAKE_LATCH_EXPIRY_NS,
        WAKE_LATCH_HELD, WAKE_LATCH_SENT, WAKE_LATCH_STAMP, WAKE_LATCH_STREAMING, WakeGate,
        gated_output_wake, judge_output_wake, output_latch_after_pass, release_output_hold,
        wake_stranded,
    };
    use std::sync::atomic::{AtomicU64, Ordering};

    const MS: u64 = 1_000_000;
    const US: u64 = 1_000;

    #[test]
    fn a_burst_streams_after_a_frame_and_a_quiet_frame_ends_it() {
        let mut c = PostCadence::default();
        assert!(!c.streaming(5 * MS), "a reader's first post");
        let mut t = 5 * MS;
        c.posted(t);
        assert!(!c.streaming(t + OUTPUT_CADENCE_NS));
        t += OUTPUT_CADENCE_NS;
        c.posted(t);
        let from = t;
        while t + 2 * MS < from + STREAM_AFTER_NS {
            t += 2 * MS;
            assert!(!c.streaming(t), "+{} µs into the burst", (t - from) / US);
            c.posted(t);
        }
        t += 2 * MS;
        assert!(c.streaming(t), "a frame into the burst");
        c.posted(t);
        t += 30 * MS;
        c.held(t);
        assert!(
            c.streaming(t + STREAM_AFTER_NS - 1),
            "a stream that stalled under a frame resumes as one"
        );
        assert!(
            !c.streaming(t + STREAM_AFTER_NS),
            "a quiet frame ends the burst"
        );
        c.posted(t + STREAM_AFTER_NS);
        assert!(!c.streaming(t + STREAM_AFTER_NS + 100 * US), "a new burst");
    }

    #[test]
    fn a_streaming_post_takes_a_hold_and_a_lone_one_clears() {
        let lone = AtomicU64::new(0);
        assert_eq!(
            gated_output_wake(&lone, 10 * MS, 0, false, || true, || 10 * MS),
            WakeGate::Posted
        );
        let word = lone.load(Ordering::Relaxed);
        assert_eq!(word, (10 * MS) | WAKE_LATCH_SENT);
        assert_eq!(
            output_latch_after_pass(word, 10 * MS),
            0,
            "a lone pass clears"
        );

        let streaming = AtomicU64::new(0);
        let mut during = 0;
        assert_eq!(
            gated_output_wake(
                &streaming,
                20 * MS,
                0,
                true,
                || {
                    during = streaming.load(Ordering::Relaxed);
                    true
                },
                || 20 * MS + 1,
            ),
            WakeGate::Posted
        );
        assert_eq!(during, (20 * MS) | WAKE_LATCH_STREAMING, "the unsent arm");
        let word = streaming.load(Ordering::Relaxed);
        assert_eq!(word, (20 * MS + 1) | WAKE_LATCH_SENT | WAKE_LATCH_STREAMING);
        let held = output_latch_after_pass(word, 20 * MS + 50 * US);
        assert_eq!(
            held,
            WAKE_LATCH_HELD | (20 * MS + 50 * US + OUTPUT_CADENCE_NS)
        );
        assert_eq!(
            held & WAKE_LATCH_SENT,
            0,
            "a hold is never a sent wake, so the watchdog never judges it"
        );
        assert_eq!(
            output_latch_after_pass(held | WAKE_LATCH_DIRTY, 30 * MS),
            WAKE_LATCH_HELD | (30 * MS + OUTPUT_CADENCE_NS)
        );
    }

    #[test]
    fn a_held_latch_coalesces_into_its_release() {
        let due = 50 * MS;
        let latch = AtomicU64::new(WAKE_LATCH_HELD | due);
        assert_eq!(
            release_output_hold(&latch, due - 1),
            HoldRelease::Pending { due_ns: due }
        );
        let mut sends = 0;
        for at in [47 * MS, 48 * MS, 49 * MS] {
            let gate = gated_output_wake(
                &latch,
                at,
                0,
                true,
                || {
                    sends += 1;
                    true
                },
                || at,
            );
            assert_eq!(gate, WakeGate::Held);
        }
        assert_eq!(sends, 0, "a hold posts nothing");
        assert_eq!(
            latch.load(Ordering::Relaxed),
            WAKE_LATCH_HELD | WAKE_LATCH_DIRTY | due
        );
        assert_eq!(
            release_output_hold(&latch, due - 1),
            HoldRelease::Pending { due_ns: due },
            "dirty, but not due"
        );
        assert_eq!(release_output_hold(&latch, due), HoldRelease::Due);
        let word = latch.load(Ordering::Relaxed);
        latch.store(output_latch_after_pass(word, due), Ordering::Relaxed);
        let due2 = due + OUTPUT_CADENCE_NS;
        assert_eq!(latch.load(Ordering::Relaxed), WAKE_LATCH_HELD | due2);
        assert_eq!(release_output_hold(&latch, due2), HoldRelease::Released);
        assert_eq!(latch.load(Ordering::Relaxed), 0);
        assert_eq!(release_output_hold(&latch, due2), HoldRelease::NotHeld);
        assert_eq!(
            gated_output_wake(&latch, due2 + MS, 0, false, || true, || due2 + MS),
            WakeGate::Posted
        );
        assert_eq!(release_output_hold(&latch, u64::MAX), HoldRelease::NotHeld);
    }

    #[test]
    fn the_watchdog_never_judges_a_hold() {
        let mut watch = OutputWakeWatch::default();
        for word in [
            WAKE_LATCH_HELD | (5 * MS),
            WAKE_LATCH_HELD | WAKE_LATCH_DIRTY | (5 * MS),
        ] {
            assert_eq!(
                judge_output_wake(word, u64::MAX, 900 * MS),
                OutputWakeVerdict::Clear
            );
            assert!(!wake_stranded(word, 1, 900 * MS));
            let latch = AtomicU64::new(word);
            watch.pass(1_000 * MS, [(4, &latch)]);
            let pass = watch.pass(2_000 * MS, [(4, &latch)]);
            assert_eq!((pass.healed, pass.late, pass.recheck_ns), (0, 0, None));
            assert_eq!(latch.load(Ordering::Relaxed), word, "untouched");
        }
        let lost = AtomicU64::new((5 * MS) | WAKE_LATCH_SENT | WAKE_LATCH_STREAMING);
        let mut watch = OutputWakeWatch::default();
        watch.pass(10 * MS, [(4, &lost)]);
        assert_eq!(watch.pass(20 * MS, [(4, &lost)]).healed, 1);
        assert_eq!(lost.load(Ordering::Relaxed), 0);
        assert!(
            wake_stranded(
                (5 * MS) | WAKE_LATCH_SENT | WAKE_LATCH_STREAMING,
                1,
                900 * MS
            ),
            "the streaming bit is not part of the stamp"
        );
        assert_eq!(WAKE_LATCH_STAMP & (WAKE_LATCH_HELD | WAKE_LATCH_DIRTY), 0);
    }

    fn drive(reads: &[u64]) -> (Vec<u64>, usize, u64) {
        drive_with_edges(reads, &[])
    }

    fn drive_with_edges(reads: &[u64], edges: &[u64]) -> (Vec<u64>, usize, u64) {
        fn pass(latch: &AtomicU64, now: u64, passes: &mut Vec<u64>) {
            let word = latch.load(Ordering::Relaxed);
            latch.store(output_latch_after_pass(word, now), Ordering::Relaxed);
            passes.push(now);
        }
        let latch = AtomicU64::new(0);
        let mut cadence = PostCadence::default();
        let (mut passes, mut folds) = (Vec::new(), 0);
        let (mut queued, mut deadline): (Option<u64>, Option<u64>) = (None, None);
        for &read in reads.iter().chain(std::iter::once(&u64::MAX)) {
            loop {
                let wake_turn = queued.filter(|&q| deadline.is_none_or(|d| q <= d));
                let Some(now) = wake_turn.or(deadline).filter(|&at| at <= read) else {
                    break;
                };
                if wake_turn.is_some() {
                    queued = None;
                    pass(&latch, now, &mut passes);
                }
                deadline = match release_output_hold(&latch, now) {
                    HoldRelease::Pending { due_ns } => Some(due_ns),
                    HoldRelease::Due => {
                        pass(&latch, now, &mut passes);
                        Some(now + OUTPUT_CADENCE_NS)
                    }
                    HoldRelease::NotHeld | HoldRelease::Released => None,
                };
                folds += usize::from(deadline.is_some());
            }
            if read == u64::MAX {
                break;
            }
            if edges.contains(&read) {
                cadence.foreground_edge(&latch);
            }
            let gate =
                gated_output_wake(&latch, read, 0, cadence.streaming(read), || true, || read);
            match gate {
                WakeGate::Posted | WakeGate::Reposted => {
                    cadence.posted(read);
                    queued = Some(read + 20 * US);
                }
                WakeGate::Held => cadence.held(read),
                WakeGate::Coalesced => {}
            }
        }
        (passes, folds, latch.load(Ordering::Relaxed))
    }

    fn assert_covered(reads: &[u64], passes: &[u64], bound: u64) {
        for &read in reads {
            let pass = passes.iter().find(|&&p| p > read).copied();
            assert!(
                pass.is_some_and(|p| p - read <= bound),
                "the chunk fed at {} µs waits for {pass:?}",
                read / US
            );
        }
    }

    #[test]
    fn a_line_per_read_writer_costs_one_pass_per_cadence() {
        let start = 1_000 * MS;
        let reads: Vec<u64> = (0..2_000).map(|i| start + i * 100 * US).collect();
        let (passes, folds, latch) = drive(&reads);
        let bound = (STREAM_AFTER_NS / (100 * US)
            + (200 * MS - STREAM_AFTER_NS) / OUTPUT_CADENCE_NS
            + 4) as usize;
        assert!(
            passes.len() <= bound,
            "{} passes for {} reads (at most {bound})",
            passes.len(),
            reads.len()
        );
        assert!(folds > 0, "the holds folded their deadlines");
        assert_covered(&reads, &passes, OUTPUT_CADENCE_NS);
        assert_eq!(latch, 0, "quiet: nothing held");
    }

    #[test]
    fn a_stream_whose_writer_stalls_keeps_its_holds() {
        let start = 1_000 * MS;
        let reads: Vec<u64> = (0..4_000u64)
            .map(|i| start + i * 100 * US + (i / 200) * 6 * MS)
            .collect();
        let (passes, folds, latch) = drive(&reads);
        let span = reads.last().copied().unwrap_or(start) - start;
        let stalls = (reads.len() / 200) as u64;
        let bound =
            (STREAM_AFTER_NS / (100 * US) + span / OUTPUT_CADENCE_NS + 2 * stalls + 4) as usize;
        assert!(
            passes.len() <= bound,
            "{} passes for {} reads (at most {bound})",
            passes.len(),
            reads.len()
        );
        assert!(folds > 0, "the holds folded their deadlines");
        assert_covered(&reads, &passes, OUTPUT_CADENCE_NS);
        assert_eq!(latch, 0, "quiet: nothing held");
    }

    #[test]
    fn a_dense_burst_streams_after_its_posts_and_an_echo_never_does() {
        let mut c = PostCadence::default();
        let from = 5 * MS;
        let mut t = from;
        for post in 0..STREAM_AFTER_POSTS {
            assert!(!c.streaming(t), "post {post}");
            c.posted(t);
            t += 100 * US;
        }
        assert!(c.streaming(t), "the post after the dense count");
        assert!(t - from < STREAM_AFTER_NS, "well short of a frame");
        let quiet = t - 100 * US + STREAM_AFTER_NS - 1;
        assert!(
            c.streaming(quiet),
            "a dense burst outlives a stall under a frame"
        );

        let mut echo = PostCadence::default();
        let mut t = 900 * MS;
        for _ in 0..7 {
            assert!(!echo.streaming(t));
            echo.posted(t);
            t += 40 * US;
        }
        assert!(!echo.streaming(t), "seven reads are no flood");
        assert!(
            !echo.streaming(t - 40 * US + OUTPUT_CADENCE_NS),
            "a quiet cadence ends a burst that is neither old nor dense"
        );
    }

    #[test]
    fn a_stream_that_stalls_before_its_first_frame_still_holds() {
        let start = 1_000 * MS;
        let reads: Vec<u64> = (0..4_000u64)
            .map(|i| start + i * 100 * US + (i / 80) * 5 * MS)
            .collect();
        let (passes, folds, latch) = drive(&reads);
        let span = reads.last().copied().unwrap_or(start) - start;
        let stalls = (reads.len() / 80) as u64;
        let bound =
            (u64::from(STREAM_AFTER_POSTS) + span / OUTPUT_CADENCE_NS + 2 * stalls + 4) as usize;
        assert!(
            passes.len() <= bound,
            "{} passes for {} reads (at most {bound})",
            passes.len(),
            reads.len()
        );
        assert!(folds > 0, "the holds folded their deadlines");
        assert_covered(&reads, &passes, OUTPUT_CADENCE_NS);
        assert_eq!(latch, 0, "quiet: nothing held");
    }

    #[test]
    fn a_prompt_after_a_program_s_output_is_never_held() {
        let start = 1_000 * MS;
        let flood: Vec<u64> = (0..400u64).map(|i| start + i * 100 * US).collect();
        let lines: Vec<u64> = (0..40u64).map(|i| start + i * 300 * US).collect();
        for (output, gap) in [(&flood, MS), (&lines, 500 * US), (&lines, 6 * MS)] {
            let at = output.last().copied().unwrap_or(start) + gap;
            let prompt: Vec<u64> = (0..3).map(|r| at + r * 200 * US).collect();
            let reads: Vec<u64> = output.iter().chain(&prompt).copied().collect();
            let (passes, _, latch) = drive_with_edges(&reads, &[at]);
            assert_covered(&prompt, &passes, 20 * US);
            assert_eq!(latch, 0, "quiet: nothing held");
        }
    }

    #[test]
    fn a_typed_echo_is_never_held() {
        let reads: Vec<u64> = (0..40)
            .flat_map(|key| (0..7).map(move |r| 1_000 * MS + key * 80 * MS + r * 40 * US))
            .collect();
        let (passes, folds, latch) = drive(&reads);
        assert_eq!(passes.len(), reads.len(), "a pass per read, as before");
        assert_eq!(folds, 0, "no hold, so no extra turn");
        assert_covered(&reads, &passes, 20 * US);
        assert_eq!(latch, 0);
    }

    #[test]
    fn the_app_runs_a_dirty_holds_pass_at_its_release() {
        use std::time::{Duration, Instant};
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        let epoch = app.lat_epoch;
        let ns = |t: Instant| u64::try_from(t.duration_since(epoch).as_nanos()).expect("ns");
        let latch = app
            .pool
            .get(0)
            .expect("session 0")
            .output_wake_pending
            .clone();
        let t0 = Instant::now();
        latch.store(
            ns(t0) | WAKE_LATCH_SENT | WAKE_LATCH_STREAMING,
            Ordering::Relaxed,
        );
        app.on_output_wake(0, t0);
        let due = ns(t0) + OUTPUT_CADENCE_NS;
        assert_eq!(latch.load(Ordering::Relaxed), WAKE_LATCH_HELD | due);
        assert_eq!(
            app.release_output_holds(t0).map(ns),
            Some(due),
            "a standing hold folds its due instant"
        );
        assert_eq!(
            crate::spawn::gated_output_wake_for_test(&latch, ns(t0) + 1, || {
                panic!("a hold posts nothing")
            }),
            WakeGate::Held
        );
        app.windows
            .get_mut(&wid)
            .expect("window 0")
            .pending_deco_birth = None;
        let at_due = epoch + Duration::from_nanos(due);
        assert_eq!(
            app.release_output_holds(at_due).map(ns),
            Some(due + OUTPUT_CADENCE_NS),
            "the release's pass holds again"
        );
        assert_eq!(
            app.windows[&wid].pending_deco_birth,
            Some(at_due),
            "the release ran the session's pass"
        );
        assert_eq!(
            latch.load(Ordering::Relaxed),
            WAKE_LATCH_HELD | (due + OUTPUT_CADENCE_NS)
        );
        app.windows
            .get_mut(&wid)
            .expect("window 0")
            .pending_deco_birth = None;
        let at_due2 = at_due + Duration::from_nanos(OUTPUT_CADENCE_NS);
        assert_eq!(app.release_output_holds(at_due2), None);
        assert_eq!(latch.load(Ordering::Relaxed), 0);
        assert_eq!(app.windows[&wid].pending_deco_birth, None);
        latch.store(ns(at_due2) | WAKE_LATCH_SENT, Ordering::Relaxed);
        app.on_output_wake(0, at_due2);
        assert_eq!(latch.load(Ordering::Relaxed), 0);
        assert_eq!(app.release_output_holds(at_due2), None);
    }

    #[test]
    fn failed_send_resets_the_flag() {
        let flag = AtomicU64::new(0);
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        drop(rx);
        let gate = gated_output_wake(&flag, 1, 0, false, || tx.send(()).is_ok(), || 2);
        assert_eq!(gate, WakeGate::Coalesced, "nothing was posted");
        assert_eq!(
            flag.load(Ordering::Relaxed),
            0,
            "failed send must not leave the latch armed"
        );
        let (tx2, rx2) = std::sync::mpsc::channel::<()>();
        let gate = gated_output_wake(&flag, 3, 0, false, || tx2.send(()).is_ok(), || 4);
        assert_eq!(gate, WakeGate::Posted);
        assert_eq!(rx2.try_iter().count(), 1);
    }

    #[test]
    fn the_arm_is_stamped_after_the_post() {
        let flag = AtomicU64::new(0);
        let mut during = 0;
        gated_output_wake(
            &flag,
            10 * MS,
            0,
            false,
            || {
                during = flag.load(Ordering::Relaxed);
                true
            },
            || 11 * MS,
        );
        assert_eq!(during, 10 * MS, "unsent while the post is in progress");
        assert_eq!(flag.load(Ordering::Relaxed), (11 * MS) | WAKE_LATCH_SENT);
    }

    #[test]
    fn a_handler_clear_during_the_post_stands() {
        let flag = AtomicU64::new(0);
        gated_output_wake(
            &flag,
            5,
            0,
            false,
            || {
                flag.store(0, Ordering::Relaxed);
                true
            },
            || 6,
        );
        assert_eq!(flag.load(Ordering::Relaxed), 0);
    }

    fn reachable(m: &aterm_spec::derive::Model) -> Vec<aterm_spec::interp::State> {
        let mut frontier = vec![m.init_state()];
        let mut seen = std::collections::BTreeSet::new();
        seen.insert(frontier[0].clone());
        let mut out = Vec::new();
        while let Some(st) = frontier.pop() {
            for a in &m.actions {
                for nxt in m.successors(a.name, &st) {
                    if seen.insert(nxt.clone()) {
                        frontier.push(nxt);
                    }
                }
            }
            out.push(st);
        }
        out
    }

    const K: u64 = 30_000_000;
    const EXPIRY_TICKS: u64 = 3; // the model's `Expiry`

    fn word(st: &aterm_spec::interp::State) -> u64 {
        let latch = st["latch"] as u64;
        match (latch, st["seen"]) {
            (0, _) => 0,
            (l, 1) => (l * K - 1) | WAKE_LATCH_SENT,
            (l, _) => (l * K + 1) | WAKE_LATCH_SENT,
        }
    }

    fn idle_since(st: &aterm_spec::interp::State) -> u64 {
        if st["busy"] == 1 {
            0
        } else {
            st["drained"] as u64 * K
        }
    }

    #[test]
    fn latch_conforms_to_damage_to_present_model() {
        let m = aterm_models_core::damage_to_present_model();
        const {
            assert!(K * EXPIRY_TICKS < WAKE_LATCH_EXPIRY_NS);
            assert!(K * (EXPIRY_TICKS + 1) - 1 > WAKE_LATCH_EXPIRY_NS);
        }
        let (mut checked, mut reposts) = (0_u32, 0_u32);
        let (mut duplicate_caught, mut stranded_caught) = (false, false);
        for st in reachable(&m) {
            let fresh = m.successors("DamageFresh", &st);
            let repost = m.successors("DamageRepost", &st);
            let model_posts = fresh.first().or(repost.first());
            let before = word(&st);
            let flag = AtomicU64::new(before);
            let now_ns = st["now"] as u64 * K;
            let mut sent = false;
            let gate = gated_output_wake(
                &flag,
                now_ns,
                idle_since(&st),
                false,
                || {
                    sent = true;
                    true
                },
                || now_ns + 1,
            );
            let expect_gate = if !fresh.is_empty() {
                WakeGate::Posted
            } else if !repost.is_empty() {
                reposts += 1;
                WakeGate::Reposted
            } else {
                WakeGate::Coalesced
            };
            assert_eq!(gate, expect_gate, "gated_output_wake vs model at {st:?}");
            assert_eq!(sent, model_posts.is_some(), "send vs model at {st:?}");
            let expect = model_posts.map_or(before, word);
            assert_eq!(flag.load(Ordering::Relaxed), expect, "latch word at {st:?}");
            let at = before & !WAKE_LATCH_SENT;
            let age_sends = before == 0 || now_ns.saturating_sub(at) > WAKE_LATCH_EXPIRY_NS;
            if age_sends && model_posts.is_none() && st["inflight"] == 1 {
                duplicate_caught = true;
            }
            if before != 0 && !repost.is_empty() {
                stranded_caught = true;
            }
            checked += 1;
        }
        assert!(
            checked > 10 && reposts > 0,
            "reachable-state walk degenerate ({checked}/{reposts})"
        );
        assert!(
            duplicate_caught,
            "the age re-send must disagree with the model on a queued wake, or this \
             bind cannot catch the 2026-09-29 duplicate wake"
        );
        assert!(
            stranded_caught,
            "the never-re-send reader must disagree with the model on a stranded wake"
        );
    }

    #[test]
    fn watchdog_conforms_to_damage_to_present_model() {
        let m = aterm_models_core::damage_to_present_model();
        let (mut judged, mut idle_judged) = (0_u32, 0_u32);
        let (mut false_heal_caught, mut past_recheck_caught) = (false, false);
        for st in reachable(&m) {
            let (latch, now) = (st["latch"], st["now"] as u64 * K);
            let armed = word(&st);
            if st["busy"] == 1 {
                let verdict = judge_output_wake(armed, st["drained"] as u64 * K, now);
                let end = m.successors("TurnEnd", &st);
                assert_eq!(end.len(), 1, "TurnEnd is deterministic");
                let end = &end[0];
                let model_heals = latch != 0 && end["latch"] == 0;
                let real_heals = matches!(verdict, OutputWakeVerdict::Lost { .. });
                assert_eq!(
                    real_heals, model_heals,
                    "judge_output_wake vs model at {st:?}"
                );
                match verdict {
                    OutputWakeVerdict::Waiting { recheck_ns, .. } => {
                        assert!(recheck_ns > now, "a recheck is never in the past");
                        assert_eq!(
                            recheck_ns.div_ceil(K),
                            end["recheck"] as u64,
                            "recheck deadline vs model at {st:?}"
                        );
                        let old = (armed & !WAKE_LATCH_SENT) + WAKE_LATCH_EXPIRY_NS + 1;
                        if old <= now && old.div_ceil(K) != end["recheck"] as u64 {
                            past_recheck_caught = true;
                        }
                    }
                    OutputWakeVerdict::Lost { .. } | OutputWakeVerdict::Clear => {
                        assert_eq!(end["recheck"], 0, "no deadline at {st:?}");
                    }
                }
                let age_heals = latch != 0
                    && now.saturating_sub(armed & !WAKE_LATCH_SENT) > WAKE_LATCH_EXPIRY_NS;
                if age_heals && !model_heals && st["inflight"] == 1 {
                    false_heal_caught = true;
                }
                judged += 1;
            } else if latch != 0 && st["inflight"] == 0 {
                let seen = st["seen"] == 1;
                assert_eq!(st["recheck"] != 0, seen, "recheck only if seen at {st:?}");
                assert_eq!(
                    idle_since(&st) > (armed & !WAKE_LATCH_SENT),
                    seen,
                    "the idle stamp orders the arm as the model does at {st:?}"
                );
                idle_judged += 1;
            }
        }
        assert!(
            judged > 10 && idle_judged > 3,
            "walk degenerate ({judged}/{idle_judged})"
        );
        assert!(
            false_heal_caught,
            "the age-only verdict must disagree with the model on a queued wake, or \
             this bind cannot catch the 2026-09-29 false heal"
        );
        assert!(
            past_recheck_caught,
            "the stamp+expiry recheck must disagree with the model after a long frame"
        );
    }

    #[test]
    fn a_300ms_frame_with_an_in_flight_wake_counts_no_heal() {
        let latch = AtomicU64::new(0);
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let mut watch = OutputWakeWatch::default();
        let loop_idle = LoopIdle::default();
        let t0 = 1_000 * MS;
        assert_eq!(watch.pass(t0, [(7, &latch)]).healed, 0);
        loop_idle.went_idle(t0);
        loop_idle.turn_began();
        assert!(rx.try_recv().is_err());
        for at in [10, 120, 200, 290] {
            gated_output_wake(
                &latch,
                t0 + at * MS,
                loop_idle.since_ns(),
                false,
                || tx.send(()).is_ok(),
                || t0 + at * MS,
            );
        }
        let end = t0 + 310 * MS;
        let pass = watch.pass(end, [(7, &latch)]);
        assert_eq!(
            pass.healed, 0,
            "a queued wake is LATE, not lost: no heal, no repaint-all"
        );
        assert_eq!(pass.late, 1, "booked once as late");
        assert_eq!(
            pass.recheck_ns,
            Some(end + WAKE_LATCH_EXPIRY_NS),
            "one expiry after this pass, not in its past"
        );
        assert_eq!(
            latch.load(Ordering::Relaxed),
            (t0 + 10 * MS) | WAKE_LATCH_SENT
        );
        assert!(end - (t0 + 10 * MS) > WAKE_LATCH_EXPIRY_NS);
        loop_idle.went_idle(end);
        let gate = gated_output_wake(
            &latch,
            end + MS,
            loop_idle.since_ns(),
            false,
            || tx.send(()).is_ok(),
            || end + MS,
        );
        assert_eq!(gate, WakeGate::Coalesced);
        assert_eq!(rx.try_iter().count(), 1, "no duplicate wake was posted");
        latch.store(0, Ordering::Relaxed);
        let next = watch.pass(end + 2 * MS, [(7, &latch)]);
        assert_eq!((next.healed, next.late, next.recheck_ns), (0, 0, None));
    }

    #[test]
    fn consecutive_long_frames_never_arm_a_past_wake_heal_deadline() {
        use crate::deadline_heal::{DeadlineHealer, Heal};
        use crate::metrics::DeadlineOwner;
        use std::time::{Duration, Instant};
        let epoch = Instant::now();
        let at = |ns: u64| epoch + Duration::from_nanos(ns);
        let latch = AtomicU64::new(0);
        let mut watch = OutputWakeWatch::default();
        let mut healer = DeadlineHealer::default();
        let mut t = 1_000 * MS;
        assert_eq!(watch.pass(t, [(9, &latch)]).recheck_ns, None);
        for turn in 0..40 {
            latch.store(0, Ordering::Relaxed);
            let post = t + 5 * MS;
            assert_eq!(
                gated_output_wake(&latch, post, 0, false, || true, || post),
                WakeGate::Posted
            );
            t += 400 * MS;
            let pass = watch.pass(t, [(9, &latch)]);
            assert_eq!(pass.healed, 0, "turn {turn}: late, not lost");
            let recheck = pass
                .recheck_ns
                .expect("an outstanding wake folds a recheck");
            let (wanted, now) = (at(recheck), at(t + MS));
            assert!(
                wanted >= now,
                "turn {turn}: a recheck {} ms from the pass is a past arm",
                (i128::from(recheck) - i128::from(t)) / i128::from(MS)
            );
            assert_eq!(
                healer.heal(DeadlineOwner::WakeHeal, Some(wanted), now),
                Heal {
                    armed: Some(wanted),
                    floor: false,
                    streak: false,
                },
                "turn {turn}: the recheck passes through unclamped"
            );
        }
    }

    #[test]
    fn a_truly_lost_wake_still_heals_within_its_bound() {
        let latch = AtomicU64::new(0);
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let mut watch = OutputWakeWatch::default();
        let t0 = 1_000 * MS;
        watch.pass(t0, [(3, &latch)]);
        let sent = t0 + 10 * MS;
        gated_output_wake(&latch, sent, 0, false, || tx.send(()).is_ok(), || sent);
        assert_eq!(rx.try_iter().count(), 1);
        let pass_at = t0 + 12 * MS;
        let pass = watch.pass(pass_at, [(3, &latch)]);
        assert_eq!(
            pass.healed, 0,
            "not yet proven: no drain has begun after the post"
        );
        let recheck = pass
            .recheck_ns
            .expect("an armed latch asks the loop to look again");
        assert_eq!(
            recheck,
            pass_at + WAKE_LATCH_EXPIRY_NS,
            "one expiry after the pass that saw it"
        );
        let pass = watch.pass(recheck, [(3, &latch)]);
        assert_eq!(pass.healed, 1, "proven lost: healed");
        assert_eq!(latch.load(Ordering::Relaxed), 0);
        gated_output_wake(
            &latch,
            recheck + 1,
            0,
            false,
            || tx.send(()).is_ok(),
            || recheck + 1,
        );
        assert_eq!(rx.try_iter().count(), 1);
    }

    #[test]
    fn an_unsent_arm_is_never_presumed_lost() {
        let v = judge_output_wake(5 * MS, u64::MAX, 900 * MS);
        assert_eq!(
            v,
            OutputWakeVerdict::Waiting {
                recheck_ns: 900 * MS + 895 * MS,
                late: true
            }
        );
        let young = judge_output_wake(5 * MS, u64::MAX, 6 * MS);
        assert_eq!(
            young,
            OutputWakeVerdict::Waiting {
                recheck_ns: 6 * MS + WAKE_LATCH_EXPIRY_NS,
                late: false
            }
        );
        assert_eq!(
            judge_output_wake(0, u64::MAX, 900 * MS),
            OutputWakeVerdict::Clear
        );
        assert!(
            !wake_stranded(5 * MS, 1, 900 * MS),
            "unsent: never stranded"
        );
        assert!(wake_stranded((5 * MS) | WAKE_LATCH_SENT, 1, 900 * MS));
    }

    #[test]
    fn a_late_wake_is_booked_once() {
        let latch = AtomicU64::new((10 * MS) | WAKE_LATCH_SENT);
        let mut watch = OutputWakeWatch::default();
        let mut books = 0;
        for _ in 0..3 {
            watch.drained_through_ns = 10 * MS;
            books += watch
                .pass(10 * MS + 2 * WAKE_LATCH_EXPIRY_NS, [(1, &latch)])
                .late;
        }
        assert_eq!(books, 1);
        latch.store((20 * MS) | WAKE_LATCH_SENT, Ordering::Relaxed);
        watch.drained_through_ns = 20 * MS;
        assert_eq!(
            watch
                .pass(20 * MS + 2 * WAKE_LATCH_EXPIRY_NS, [(1, &latch)])
                .late,
            1
        );
    }
}

#[cfg(test)]
mod kitty_transfer_tests {
    use super::*;
    use std::io::Write as _;

    fn apc(control: &str, payload: &[u8]) -> Vec<u8> {
        let mut v = b"\x1b_G".to_vec();
        v.extend_from_slice(control.as_bytes());
        v.push(b';');
        v.extend_from_slice(
            aterm_codec::base64::encode(payload)
                .expect("encode")
                .as_bytes(),
        );
        v.extend_from_slice(b"\x1b\\");
        v
    }

    #[test]
    fn alternate_scroll_survives_a_foreground_handback() {
        let mut t = new_live_terminal(10, 40, None, None);
        t.process(b"\x1b[?25l");
        let h = t
            .foreground_handback()
            .expect("a hidden cursor is evidence");
        assert!(
            !h.reverted.contains(&"alternate-scroll"),
            "{:?}",
            h.reverted
        );
        assert!(t.modes().alternate_scroll, "DEC 1007 is still ON");
        t.process(b"\x1b[?1007l\x1b[?1049h");
        t.foreground_handback().expect("the alt screen is evidence");
        assert!(t.modes().alternate_scroll);
    }

    #[test]
    fn live_session_archive_carries_the_process_origin() {
        let a = new_live_terminal(10, 40, None, None);
        let b = new_live_terminal(12, 50, None, None);
        let origin = alt_archive_origin();
        assert_ne!(origin, 0, "an origin of 0 is the unset value");
        assert_eq!(origin, alt_archive_origin(), "fixed for the process");
        assert_eq!(a.alt_archive().origin(), origin);
        assert_eq!(b.alt_archive().origin(), origin);
    }

    #[test]
    fn live_session_attaches_tiered_scrollback_driven_by_config() {
        use aterm_core::config::TerminalConfig;
        let t = new_live_terminal(10, 40, None, None);
        assert!(
            t.scrollback().is_some(),
            "SCROLL-1: a live session must attach a tiered scrollback store"
        );
        let tc = TerminalConfig {
            scrollback_limit: Some(5_000),
            ..TerminalConfig::default()
        };
        let t = new_live_terminal(10, 40, Some(&tc), None);
        assert_eq!(
            t.grid().scrollback_line_limit(),
            Some(5_000),
            "SCROLL-1: scrollback_limit must drive the attached store"
        );
        let tc = TerminalConfig {
            scrollback_limit: None,
            ..TerminalConfig::default()
        };
        let t = new_live_terminal(10, 40, Some(&tc), None);
        assert!(
            t.scrollback().is_some(),
            "the store stays attached when unlimited"
        );
        assert_eq!(
            t.grid().scrollback_line_limit(),
            None,
            "SCROLL-1: unlimited (0) yields an unbounded line limit"
        );
    }

    #[test]
    fn live_session_adopts_the_os_color_scheme_at_construction() {
        use aterm_types::Appearance;
        let mut dark = new_live_terminal(10, 40, None, None);
        dark.process(b"\x1b[?996n");
        assert_eq!(dark.take_response().as_deref(), Some(&b"\x1b[?997;1n"[..]));
        let tc = aterm_core::config::TerminalConfig {
            color_scheme: Some(Appearance::Light),
            ..aterm_core::config::TerminalConfig::default()
        };
        let mut light = new_live_terminal(10, 40, Some(&tc), None);
        assert_eq!(
            light.color_scheme(),
            Appearance::Light,
            "BROKEN-2: a tab spawned under a light palette must report light"
        );
        light.process(b"\x1b[?996n");
        assert_eq!(
            light.take_response().as_deref(),
            Some(&b"\x1b[?997;2n"[..]),
            "BROKEN-2: DSR ?996n must report the live scheme, not the dark default"
        );
    }

    fn term_with_resolver() -> Arc<Mutex<Terminal>> {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        term_lock(&term).set_cell_pixel_size(10, 20);
        configure_kitty_file_transfer(&term, HostReach::Platform);
        term
    }

    #[test]
    fn a_file_medium_is_read_before_the_terminal_lock() {
        let scratch = scratch_in(&std::env::temp_dir(), "lock");
        let path = cell_file(&scratch.0, "img.rgba");
        let seq = apc(
            "a=T,f=32,s=10,v=20,i=7,t=f",
            path.to_str().unwrap().as_bytes(),
        );

        let term = term_with_resolver();
        term_lock(&term).process(&seq);
        let in_the_hold =
            String::from_utf8_lossy(&term_lock(&term).take_response().unwrap_or_default())
                .into_owned();
        assert!(
            in_the_hold.contains("EINVAL")
                && term_lock(&term).cell_frame(24, 80).images[0].is_empty(),
            "the file was read under the terminal lock: {in_the_hold:?}"
        );

        let term = term_with_resolver();
        crate::host_io_prefetch::feed_as_reader(&term, &seq);
        let reply = String::from_utf8_lossy(&term_lock(&term).take_response().unwrap_or_default())
            .into_owned();
        assert!(reply.ends_with(";OK\x1b\\"), "{reply:?}");
        assert!(!term_lock(&term).cell_frame(24, 80).images[0].is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn the_pty_reader_reads_a_file_medium_before_its_hold() {
        struct Attached {
            session: crate::Session,
            slave: i32,
        }
        impl Drop for Attached {
            fn drop(&mut self) {
                let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
                let _ = park_reader(&mut self.session, deadline);
                let master = std::mem::replace(&mut self.session.master, -1);
                aterm_pty::close_fd(self.slave);
                aterm_pty::close_fd(master);
            }
        }
        let scratch = scratch_in(&std::env::temp_dir(), "reader");
        let path = cell_file(&scratch.0, "img.rgba");
        let app = crate::App::headless_for_test();
        let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
        let mut session = crate::stub_session(91);
        session.master = master;
        term_lock(&session.term).set_cell_pixel_size(10, 20);
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
        let post = Arc::new(move |wake: crate::Wake| {
            if matches!(wake, crate::Wake::Ready { .. }) {
                let _ = ready_tx.send(());
            }
            true
        });
        attach_reader_inner(
            &mut session,
            crate::WindowId(0),
            post,
            &app.session_factory,
            None,
        )
        .expect("attach the real reader");
        let rig = Attached { session, slave };
        ready_rx
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the reader's gather started");
        let term = Arc::clone(&rig.session.term);
        let output = |bytes: &[u8], title: &str| {
            let mut out = bytes.to_vec();
            out.extend_from_slice(format!("\x1b]2;{title}\x07").as_bytes());
            let mut off = 0;
            while off < out.len() {
                // SAFETY: a write of a live buffer to the test-owned slave.
                let n =
                    unsafe { libc::write(rig.slave, out[off..].as_ptr().cast(), out.len() - off) };
                assert!(n > 0, "write the program's output");
                off += n as usize;
            }
            let shown = aterm_spec::wait::within(
                aterm_spec::wait::MUST_HAPPEN,
                std::time::Duration::from_millis(1),
                || (term_lock(&term).title() == title).then_some(()),
            );
            assert!(shown.is_some(), "the reader never fed {title:?}");
        };
        let seq = apc(
            "a=T,f=32,s=10,v=20,i=7,t=f",
            path.to_str().unwrap().as_bytes(),
        );

        output(b"", "before");
        configure_kitty_file_transfer(&term, HostReach::Platform);
        output(b"", "installed");
        output(&seq, "sent");
        assert!(
            !term_lock(&term).cell_frame(24, 80).images[0].is_empty(),
            "the reader did not read the file before its hold"
        );
    }

    struct Scratch(std::path::PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch_in(parent: &Path, tag: &str) -> Scratch {
        let dir = parent.join(format!("aterm-kft-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn outside_temp_dirs(tag: &str) -> (aterm_tempfile::TempDir, std::path::PathBuf) {
        let temps = kitty_temp_dirs();
        let target = std::env::current_exe()
            .ok()
            .and_then(|exe| Some(exe.parent()?.parent()?.to_path_buf()));
        let var_tmp = cfg!(unix).then(|| std::path::PathBuf::from("/var/tmp"));
        for parent in [target, var_tmp].into_iter().flatten() {
            let Ok(resolved) = std::fs::canonicalize(&parent) else {
                continue;
            };
            if temps.iter().any(|temp| resolved.starts_with(temp)) {
                continue;
            }
            let Ok(dir) = aterm_tempfile::Builder::new()
                .prefix(&format!("aterm-kft-{tag}-"))
                .tempdir_in(&resolved)
            else {
                continue;
            };
            let canonical = std::fs::canonicalize(dir.path()).unwrap();
            return (dir, canonical);
        }
        aterm_spec::host::could_not_run(format!(
            "no writable directory outside the temporary ones ({temps:?}): neither the \
             build's target directory nor /var/tmp"
        ))
    }

    fn cell_file(dir: &Path, name: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, vec![0u8; 10 * 20 * 4]).unwrap();
        path
    }

    fn transmit_temp_file(path: &Path) -> bool {
        let term = term_with_resolver();
        let seq = apc("a=T,f=32,s=10,v=20,t=t", path.to_str().unwrap().as_bytes());
        crate::host_io_prefetch::feed_as_reader(&term, &seq);
        !term_lock(&term).cell_frame(24, 80).images[0].is_empty()
    }

    #[test]
    fn a_temp_file_is_deleted_only_where_the_protocol_allows() {
        const MARKER: &str = "tty-graphics-protocol";
        let temp_scratch = scratch_in(&std::env::temp_dir(), "t");
        let temp = &temp_scratch.0;
        let (_outside_scratch, canonical) = outside_temp_dirs("kept");
        let outside = &canonical;

        let plain = cell_file(temp, "plain.rgba");
        let placed = transmit_temp_file(&plain);
        assert!(
            plain.exists(),
            "a t=t file without the marker is never deleted"
        );
        assert!(placed, "a t=t file without the marker is still read");

        let consumed = cell_file(temp, &format!("x-{MARKER}.rgba"));
        assert!(transmit_temp_file(&consumed), "a proper t=t file is placed");
        assert!(!consumed.exists(), "a proper t=t file is deleted");

        let kept = cell_file(outside, &format!("x-{MARKER}.rgba"));
        let placed = transmit_temp_file(&kept);
        assert!(
            kept.exists(),
            "a file outside the temporary directories is kept"
        );
        assert!(placed, "the file is still read");

        let climb = temp.join(MARKER);
        std::fs::create_dir_all(&climb).unwrap();
        let victim = cell_file(outside, "victim.rgba");
        let mut escaped = climb;
        for _ in 0..64 {
            escaped.push("..");
        }
        escaped.extend(
            canonical
                .components()
                .filter(|part| matches!(part, std::path::Component::Normal(_))),
        );
        escaped.push("victim.rgba");
        let placed = transmit_temp_file(&escaped);
        assert!(victim.exists(), "`..` cannot carry the delete out of /tmp");
        assert!(placed, "the file is still read");

        let unmarked = cell_file(temp, "unmarked.rgba");
        let lent = temp.join(MARKER).join("..").join("unmarked.rgba");
        let placed = transmit_temp_file(&lent);
        assert!(
            unmarked.exists(),
            "a passing marked directory lends no mark"
        );
        assert!(placed, "the file is still read");
    }

    #[cfg(unix)]
    #[test]
    fn a_directory_swapped_after_the_judgement_moves_no_delete() {
        const MARKER: &str = "tty-graphics-protocol";
        let temp_scratch = scratch_in(&std::env::temp_dir(), "swap");
        let (_outside_scratch, outside) = outside_temp_dirs("swap-kept");
        let marked = temp_scratch.0.join(MARKER);
        std::fs::create_dir_all(&marked).unwrap();
        let sent = cell_file(&marked, "victim.rgba");
        let victim = cell_file(&outside, "victim.rgba");
        let held = temp_scratch.0.join("held");

        let mut swapped = false;
        let read = read_kitty_file(&sent);
        consume_kitty_temp_file_with(&sent, || {
            std::fs::rename(&marked, &held).unwrap();
            std::os::unix::fs::symlink(&outside, &marked).unwrap();
            swapped = true;
        });

        assert!(read.is_some(), "the file is read");
        assert!(swapped, "the entry was judged deletable, so the swap ran");
        assert!(
            victim.exists(),
            "the unlink followed the swapped-in symlink out of the temporary directory"
        );
        assert!(
            !held.join("victim.rgba").exists(),
            "the entry judged is the entry deleted"
        );
    }

    fn transmit_reply(term: &Arc<Mutex<Terminal>>, medium: &str, path: &Path) -> String {
        let control = format!("a=t,f=32,s=10,v=20,i=9,t={medium}");
        crate::host_io_prefetch::feed_as_reader(
            term,
            &apc(&control, path.to_str().unwrap().as_bytes()),
        );
        let reply = term_lock(term).take_response().unwrap_or_default();
        String::from_utf8_lossy(&reply).into_owned()
    }

    #[test]
    fn a_contained_session_reads_nothing_its_program_names() {
        use aterm_containment::ContainmentMode as Cm;
        let scratch = scratch_in(&std::env::temp_dir(), "reach");
        let marked = scratch.0.join("tty-graphics-protocol");
        std::fs::create_dir_all(&marked).unwrap();
        let missing = scratch.0.join("absent.rgba");
        for mode in [Cm::User, Cm::Safety, Cm::Containment] {
            let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
            term_lock(&term).set_cell_pixel_size(10, 20);
            configure_kitty_file_transfer(&term, host_reach(mode));
            let file = cell_file(&scratch.0, "present.rgba");
            let temp = cell_file(&marked, "sent.rgba");
            let present = transmit_reply(&term, "f", &file);
            let absent = transmit_reply(&term, "f", &missing);
            let consumed = transmit_reply(&term, "t", &temp);
            if mode == Cm::Containment {
                for reply in [&present, &absent, &consumed] {
                    assert!(
                        reply.contains("ENOTSUPPORTED"),
                        "a contained transmission reached a medium resolver: {reply:?}"
                    );
                }
                assert_eq!(present, absent, "a contained program tells a file exists");
                assert!(temp.exists(), "a contained t=t deleted a file as aterm");
            } else {
                assert!(present.ends_with(";OK\x1b\\"), "{mode}: {present:?}");
                assert_ne!(present, absent, "{mode}");
                assert!(consumed.ends_with(";OK\x1b\\"), "{mode}: {consumed:?}");
                assert!(!temp.exists(), "{mode}: the t=t file is consumed");
            }
        }
    }

    #[test]
    fn a_temp_medium_the_engine_refuses_is_not_consumed() {
        let scratch = scratch_in(&std::env::temp_dir(), "refused");
        let marked = scratch.0.join("tty-graphics-protocol");
        std::fs::create_dir_all(&marked).unwrap();
        let path = cell_file(&marked, "sent.rgba");
        let placed =
            |term: &Arc<Mutex<Terminal>>| !term_lock(term).cell_frame(24, 80).images[0].is_empty();
        let name = path.to_str().unwrap().as_bytes();
        let term = term_with_resolver();
        crate::host_io_prefetch::feed_as_reader(&term, &apc("a=T,f=32,t=t", name));
        assert!(!placed(&term), "PRECONDITION: the engine refused it");
        assert!(path.exists(), "a t=t file the engine refused was consumed");
        crate::host_io_prefetch::feed_as_reader(&term, &apc("a=T,f=32,s=10,v=20,t=t", name));
        assert!(placed(&term), "CONTROL: the sized transmission is placed");
        assert!(!path.exists(), "CONTROL: and its t=t file consumed");

        #[cfg(unix)]
        {
            let shm = format!("/aterm-kft-{}", std::process::id());
            let cname = std::ffi::CString::new(shm.clone()).unwrap();
            let exists = || {
                // SAFETY: a valid C string; the fd, when one opens, is closed.
                let fd = unsafe { libc::shm_open(cname.as_ptr(), libc::O_RDONLY, 0) };
                if fd >= 0 {
                    unsafe { libc::close(fd) };
                }
                fd >= 0
            };
            // SAFETY: `sysconf` takes no pointers.
            let page = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).unwrap();
            // SAFETY: a valid C string; the object is sized and closed.
            unsafe {
                let fd = libc::shm_open(cname.as_ptr(), libc::O_CREAT | libc::O_RDWR, 0o600);
                assert!(fd >= 0, "the shared-memory object opens");
                assert_eq!(libc::ftruncate(fd, page as libc::off_t), 0);
                libc::close(fd);
            }
            let term = term_with_resolver();
            crate::host_io_prefetch::feed_as_reader(&term, &apc("a=T,f=32,t=s", shm.as_bytes()));
            let kept = exists();
            crate::host_io_prefetch::feed_as_reader(
                &term,
                &apc(
                    &format!("a=T,f=32,s={},v=16,t=s", page / 64),
                    shm.as_bytes(),
                ),
            );
            let taken_placed = placed(&term);
            let left = exists();
            // SAFETY: a valid C string; removing the test's own object if still there.
            unsafe { libc::shm_unlink(cname.as_ptr()) };
            assert!(kept, "a t=s object the engine refused was unlinked");
            assert!(
                taken_placed,
                "CONTROL: the sized t=s transmission is placed"
            );
            assert!(!left, "CONTROL: and its object unlinked");
        }
    }

    #[test]
    fn file_medium_rejects_oversized_file() {
        let dir = std::env::temp_dir().join(format!("aterm-kft-big-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("big.rgba");
        std::fs::File::create(&path)
            .unwrap()
            .write_all(&vec![0u8; (MAX_KITTY_MEDIUM_BYTES as usize) + 1])
            .unwrap();

        let term = term_with_resolver();
        let seq = apc("a=T,f=32,s=10,v=20,t=f", path.to_str().unwrap().as_bytes());
        crate::host_io_prefetch::feed_as_reader(&term, &seq);
        let placed = !term_lock(&term).cell_frame(24, 80).images[0].is_empty();

        let _ = std::fs::remove_dir_all(&dir);
        assert!(!placed, "an over-cap file must be rejected (fail closed)");
    }
}

#[cfg(test)]
mod adoption_render_debt_tests {
    use std::sync::{Arc, Mutex};

    use aterm_core::terminal::{RenderDebt, Terminal};

    use super::seed_adopted_render_debt;
    use crate::resize_epoch::EpochVerdict;
    use crate::session_timeline::SessionTimeline;

    fn restored() -> Arc<Mutex<Terminal>> {
        let mut t = Terminal::new(12, 20);
        t.process(b"\x1b[?1049h\x1b[2J");
        for r in 0..12 {
            t.process(format!("\x1b[{};1HL{r}", r + 1).as_bytes());
        }
        Arc::new(Mutex::new(t))
    }

    fn timeline() -> Arc<Mutex<SessionTimeline>> {
        Arc::new(Mutex::new(SessionTimeline::default()))
    }

    #[test]
    fn a_seeded_debt_opens_an_epoch_that_is_judged_from_attach() {
        let term = restored();
        let tl = timeline();
        seed_adopted_render_debt(&term, &tl, RenderDebt::of_rows(-1, 0));
        assert_eq!(
            term.lock().unwrap().render_debt(),
            RenderDebt::of_rows(-1, 0)
        );
        let probe = {
            let t = term.lock().unwrap();
            crate::resize_ledger::EngineProbe::sample(&t)
        };
        let mut guard = tl.lock().unwrap();
        guard.set_agent_reader_for_test(true);
        let epoch = guard
            .resizes()
            .epochs()
            .current()
            .cloned()
            .expect("an epoch");
        assert_eq!(epoch.verdict, EpochVerdict::Open, "inside the risk wait");
        assert_eq!((epoch.shift, epoch.pushed), (-1, 0));
        assert!(guard.resize_nudge_view().is_none());
        let after = epoch.last_resize_ms + crate::resize_ledger::RISK_AFTER_MS;
        guard.evaluate_resizes(probe, after + 1);
        let view = guard.resize_nudge_view().expect("at risk, agent, untried");
        assert_eq!((view.shift, view.pushed), (-1, 0));
    }

    #[test]
    fn a_seeded_cut_is_judged_from_attach_as_rows_are() {
        let term = restored();
        let tl = timeline();
        let cut = RenderDebt {
            cut: 2,
            ..RenderDebt::default()
        };
        seed_adopted_render_debt(&term, &tl, cut);
        assert_eq!(term.lock().unwrap().render_debt(), cut);
        let probe = {
            let t = term.lock().unwrap();
            crate::resize_ledger::EngineProbe::sample(&t)
        };
        let mut guard = tl.lock().unwrap();
        guard.set_agent_reader_for_test(true);
        let epoch = guard
            .resizes()
            .epochs()
            .current()
            .cloned()
            .expect("an epoch on a cut alone");
        let after = epoch.last_resize_ms + crate::resize_ledger::RISK_AFTER_MS;
        guard.evaluate_resizes(probe, after + 1);
        let view = guard.resize_nudge_view().expect("at risk, agent, untried");
        assert_eq!((view.shift, view.pushed, view.cut), (0, 0, 2));
    }
}

#[cfg(test)]
mod askpass_env_tests {
    use super::askpass_env;

    const BUNDLE: &str = "/Applications/aterm.app/Contents/MacOS";

    #[test]
    fn sets_both_when_unset_in_a_bundle() {
        if !cfg!(target_os = "macos") {
            return;
        }
        let env = askpass_env(Some(BUNDLE), false, None, None);
        assert!(env.iter().any(|(k, v)| k == "SUDO_ASKPASS"
            && v == "/Applications/aterm.app/Contents/MacOS/aterm-askpass"));
        assert!(env.iter().any(|(k, v)| k == "DISPLAY" && v.is_empty()));
    }

    #[test]
    fn keeps_the_users_own_values() {
        if !cfg!(target_os = "macos") {
            return;
        }
        let env = askpass_env(Some(BUNDLE), false, Some("/opt/my/askpass"), Some(":0"));
        assert!(env.iter().all(|(k, _)| k != "SUDO_ASKPASS"), "{env:?}");
        assert!(env.iter().all(|(k, _)| k != "DISPLAY"), "{env:?}");
    }

    #[test]
    fn replaces_an_enclosing_aterms_helper() {
        if !cfg!(target_os = "macos") {
            return;
        }
        let env = askpass_env(
            Some(BUNDLE),
            false,
            Some("/old/aterm.app/Contents/MacOS/aterm-askpass"),
            Some(":0"),
        );
        assert_eq!(
            env.iter()
                .find(|(k, _)| k == "SUDO_ASKPASS")
                .map(|(_, v)| v.as_str()),
            Some("/Applications/aterm.app/Contents/MacOS/aterm-askpass")
        );
    }

    #[test]
    fn nothing_without_a_bundle_or_when_contained() {
        assert!(askpass_env(None, false, None, None).is_empty());
        assert!(askpass_env(Some(BUNDLE), true, None, None).is_empty());
    }
}
