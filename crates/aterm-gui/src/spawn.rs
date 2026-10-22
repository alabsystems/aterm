// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Session spawning and recursion provisioning: `spawn_session` builds a tab's
//! engine and PTY from a `SessionFactory`, prepares shell integration, and
//! provisions a child aterm's recursion identity and edges. Also
//! `App::register_session`.

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

/// The shell-integration loader's load-once guard variable, which the shipped
/// zsh/bash scripts check and then export.
///
/// A nested aterm inherits the parent shell's exported guard, so its loader
/// would bail and emit no OSC 133/633 marks. The spawn env assembly in `lib.rs`
/// overrides it with an empty string for every integrated session; a test pins
/// this name against the shipped scripts.
pub(crate) const SHELL_INTEGRATION_LOADED_GUARD: &str = "ATERM_SHELL_INTEGRATION_INSTALLED";

/// The runtime outcome of the most recent shell-integration preparation, as
/// opposed to the advertised capability constant.
///
/// A tab gets `<prefix>/bin` on PATH, with `agents/` and `reroute/` kept in
/// front after its rc ran, only because the integration script's live path
/// (`__aterm_managed_path_live`, every precmd and preexec) sources
/// `~/.aterm/shell.d` and re-asserts the order. When preparation fails (an
/// unknown shell, an unwritable loader cache) that does not happen, so the
/// toolchain pill and `--diagnose` read this record instead of assuming success.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ShellIntegrationOutcome {
    /// The loader was written and injected: every tab of this window sources
    /// `shell.d` at its next prompt, so the ALab tools' PATH hook reaches them.
    Prepared,
    /// No integration script exists for this shell (named as selected/`$SHELL`)
    /// — nothing will ever source `shell.d` in it.
    UnknownShell(String),
    /// The loader cache could not be written (the `std::io::Error` text).
    WriteFailed(String),
    /// Config `shell_args` ask for a start no integration can stand in for (a
    /// command, a script, POSIX mode, …): the shell runs them as given, without
    /// integration. The sentence says which argument and why
    /// (`aterm_shell_integration::ArgsMerge::Bypass`).
    ArgsBypass(String),
}

/// Last-write-wins record of the most recent preparation attempt. One slot is
/// enough: every tab of a session uses the same selected shell and the same
/// loader cache, so the newest attempt is the current truth.
static SHELL_INTEGRATION_OUTCOME: std::sync::Mutex<Option<ShellIntegrationOutcome>> =
    std::sync::Mutex::new(None);

/// The recorded runtime outcome, `None` before any integrated spawn was
/// attempted (e.g. an `-e <cmd>`-only run, where no promise is broken because
/// no interactive shell exists to type into).
pub(crate) fn shell_integration_outcome() -> Option<ShellIntegrationOutcome> {
    SHELL_INTEGRATION_OUTCOME
        .lock()
        .expect("shell integration outcome lock")
        .clone()
}

/// Every outcome this process has recorded, so a failure is warned about once:
/// a `/bin/sh` user's aterm.log held one identical line per tab.
static SHELL_INTEGRATION_SEEN: std::sync::Mutex<Vec<ShellIntegrationOutcome>> =
    std::sync::Mutex::new(Vec::new());

/// Record `outcome` as the current truth (every time: the pill and
/// `--diagnose` read the newest), and answer whether it is new to this process
/// — the one time a failure is worth a warning.
fn record_shell_integration_outcome(outcome: ShellIntegrationOutcome) -> bool {
    let first = first_sighting(&SHELL_INTEGRATION_SEEN, &outcome);
    *SHELL_INTEGRATION_OUTCOME
        .lock()
        .expect("shell integration outcome lock") = Some(outcome);
    first
}

/// Whether `seen` lacked `outcome`, adding it when it did.
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

// Auto-prime: a windowed aterm's launch and every fresh session spawn run the
// coding-agent primer (`aterm agents install`) on a detached thread, throttled
// and fail-soft, so an agent launched anywhere knows what aterm is — and knows
// it in this build's words, in a tab the launch adopted or restored too, under
// the user's configured agent home. No pass starts before the launch's first
// frame is on glass ([`PrimeSchedule`]). A seamless
// update's candidate writes nothing before its Commit. `agents_auto_prime =
// false` turns it off.

/// How often one process re-runs the installer at most. A spawn burst (a
/// restore of twenty tabs) must cost one pass, not twenty; and after the first
/// pass every later one is a no-op re-read, so a minute loses nothing.
const AGENT_PRIME_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// This process's primer schedule: the launch's hold, then the throttle.
/// Stamped before any thread runs, so a burst of spawns inside the window
/// never launches a second thread.
static AGENT_PRIME: Mutex<PrimeSchedule> = Mutex::new(PrimeSchedule::AT_LAUNCH);

/// Set by the first [`release_held_prime`], so every park after it costs one
/// load.
static AGENT_PRIME_RELEASED: AtomicBool = AtomicBool::new(false);

/// What asks for a primer pass ([`prime_agents_if_due`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PrimeCue {
    /// A windowed aterm's launch, once it holds the sessions it will run and
    /// before the first of them is spawned or adopted. Every tab of a seamless
    /// update's successor, and of a launch that reattached the keeper's shells,
    /// is a shell this build did not spawn: the context files its agents read
    /// were written by the build before, whose primer an update supersedes.
    /// Its pass stands in for session 0's fresh spawn, which finds the
    /// throttle taken.
    Launch,
    /// A session spawn: `adopted` when it re-adopts a running shell (a
    /// seamless update's handoff, the keeper), `spare` when it is a warm spare
    /// nobody has asked for yet ([`crate::spare`]).
    Spawn { adopted: bool, spare: bool },
    /// A warm spare adopted into a tab: its spawn did not ask.
    SpareAdopted,
}

/// Whether `cue` asks for a pass, pure. The launch does, so an agent in a tab
/// it adopted (or one the harness relaunches there with `--resume`) reads this
/// build's primer; a fresh spawn does, so a context file deleted or edited
/// since is put back; a spare does at its adoption, not at its spawn. A
/// re-adopted shell does not: its process's launch asked just before. The
/// gates ([`agent_prime_permitted`]), the throttle ([`due`]) and the knob come
/// after this.
const fn cue_primes(cue: PrimeCue) -> bool {
    match cue {
        PrimeCue::Launch | PrimeCue::SpareAdopted => true,
        PrimeCue::Spawn { adopted, spare } => !adopted && !spare,
    }
}

/// Set once, at launch, when this process runs `--headless`
/// ([`mark_headless_instance`]). A headless aterm is a test or an agent's
/// private instance, often launched with the human's real `$HOME` still in
/// its environment; the primer writes agent context files under `$HOME`, so
/// a headless instance never runs it.
static HEADLESS_INSTANCE: AtomicBool = AtomicBool::new(false);

/// Record that this process is a headless instance. Called by `main` once the
/// mode is decided, before the first session spawns.
pub(crate) fn mark_headless_instance() {
    HEADLESS_INSTANCE.store(true, Ordering::Release);
}

/// Whether a cue ([`cue_primes`]) may run the primer at all, pure: never in a
/// unit test (none may write the developer's real `$HOME`), never in a
/// headless instance. The throttle and the `agents_auto_prime` knob come after
/// this.
const fn agent_prime_permitted(test_build: bool, headless: bool) -> bool {
    !test_build && !headless
}

/// The result of one auto-prime pass — what actually happened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AgentPrimeOutcome {
    /// `agents_auto_prime = false`: nothing was read or written.
    Disabled,
    /// `$HOME` is unset or empty: nowhere to look.
    NoHome,
    /// This process runs from an app bundle that is not the release `aterm.app` (a dev
    /// bundle from `ship dev-app`, a renamed copy): nothing was read or written. The
    /// agents' context files are shared user state; a dev build primes them only when
    /// asked (`aterm agents install`). See `atpkg::hooks::runs_from_non_release_bundle`.
    NotReleaseBundle,
    /// The installer ran; its per-agent result and one-line summary.
    Ran(aterm_primer::AutoPrime),
}

/// The throttle decision, pure: due when nothing ran yet, or when the last pass
/// started at least [`AGENT_PRIME_INTERVAL`] ago. `now` is injected so a test
/// walks the clock instead of sleeping; a clock that went backwards is simply
/// "not due", never a panic.
fn due(last: Option<Instant>, now: Instant) -> bool {
    match last {
        None => true,
        Some(started) => now.saturating_duration_since(started) >= AGENT_PRIME_INTERVAL,
    }
}

/// Whether `cue` starts a pass at `now`, past the process's gates: it asks
/// ([`cue_primes`]) and the throttle is [`due`], and then `last` is stamped,
/// pure over the stamp so a test walks a launch's cues on its own clock.
fn take_pass(cue: PrimeCue, last: &mut Option<Instant>, now: Instant) -> bool {
    if !cue_primes(cue) || !due(*last, now) {
        return false;
    }
    *last = Some(now);
    true
}

/// Where the launch's hold stands ([`PrimeSchedule`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PrimeHold {
    /// Before the launch's first frame is on glass: the pass a cue took
    /// meanwhile, waiting to start.
    Holding(Option<PrimeCue>),
    /// Since: a pass starts when its cue takes it.
    Released,
}

/// When passes start, pure over its state so a test walks a launch on its own
/// clock: the throttle ([`take_pass`]) and the launch's hold. A pass taken
/// before the launch's first frame is on glass waits for that frame: started
/// at the cue, its thread, its config read and the home's disk ran beside
/// session 0's fork, the event loop's build and the shell's start. It starts
/// from the first park after the frame ([`release_held_prime`]). A seamless
/// update's successor gets there before its Commit, since its readiness proof
/// is that frame, so its pass still waits for the Commit on its thread
/// ([`handoff_committed`]) and writes within one poll of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PrimeSchedule {
    /// When the installer was last taken (`None` before the launch's pass).
    last: Option<Instant>,
    hold: PrimeHold,
}

impl PrimeSchedule {
    /// A process's schedule at its launch: nothing taken, held.
    const AT_LAUNCH: Self = Self {
        last: None,
        hold: PrimeHold::Holding(None),
    };

    /// The pass `cue` starts at `now`. `None` when it asks for none or the
    /// throttle is not due ([`take_pass`]), or when the hold keeps it for
    /// [`Self::release`]; a launch's held pass is kept over any other, being a
    /// spawn's and more.
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

    /// The launch's first frame is on glass: the pass the hold kept, once.
    fn release(&mut self) -> Option<PrimeCue> {
        match std::mem::replace(&mut self.hold, PrimeHold::Released) {
            PrimeHold::Holding(held) => held,
            PrimeHold::Released => None,
        }
    }
}

/// One pass with its inputs injected (the knob, the home), so a test drives it
/// against a scratch directory: runs the installer, returns the outcome, logs
/// a change at `info` and each failed agent at `warn` — and nothing when the
/// pass changed nothing, because a line per spawn would be noise.
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

/// Run the primer only after the seamless handoff commits.
fn agent_prime_pass(
    committed: impl FnOnce() -> bool,
    prime: impl FnOnce() -> AgentPrimeOutcome,
) -> Option<AgentPrimeOutcome> {
    committed().then(prime)
}

/// Whether this process may write the agents' files, waiting until it may: a
/// launch that carries a seamless handoff is a candidate the outgoing process
/// can still reject until its Commit, and a newer primer it wrote would then
/// stay under the build that kept the sessions, which never rolls a newer block
/// back. So a candidate's pass waits for its Commit, as its other boot lanes do
/// ([`aterm_update::wait_while_uncommitted_handoff_candidate`]); one still
/// uncommitted past the bound has been told to exit, and writes nothing. Any
/// other process answers at once.
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

/// Prime detected coding agents if `cue` starts a pass
/// ([`PrimeSchedule::admit`]); asked at the launch, at the top of every
/// [`spawn_session`] and at a spare's adoption. A pass taken before the
/// launch's first frame is on glass starts at [`release_held_prime`].
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

/// The launch's first frame is on glass (a headless instance, which has none,
/// asks at its first park): start the pass the hold kept, if a cue took one.
/// Asked at every park (`App::about_to_wait`); one load once released.
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

/// Run `cue`'s pass on a detached thread, because the installer does disk I/O
/// the event loop must not wait on, and a candidate's pass waits there for its
/// Commit ([`handoff_committed`]). The handle is dropped: nothing joins it, and
/// the pass writes whole files only, so an agent starting meanwhile reads the
/// block before it or the one after, never a torn file.
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

/// Env additions, optional argv override, and the raw capability nonce that
/// [`prepare_shell_integration`] hands back to the spawn path.
type ShellIntegrationSetup = (
    Vec<(String, String)>,
    Option<Vec<String>>,
    [u8; 32],
    aterm_core::shell_integration::ShellType,
);

/// The shell this tab will actually spawn, as the integration layer sees it.
///
/// Detected from the program the spawn itself resolves
/// ([`aterm_pty::select_shell_with_origin`], over config `shell` / `--shell`),
/// so every shell gets its own integration script. On Unix, with `$SHELL`
/// unset, that is the account's login shell, which must get bash's integration
/// when it is bash, and never bash's `--rcfile` when it is not. On Windows, with
/// neither PowerShell on PATH, it is `%COMSPEC%`'s cmd.exe, which gets cmd's
/// prompt marks: a platform guess of PowerShell gave that cmd.exe pwsh's
/// `-NoExit -Command` line and no marks at all. An alias resolves first too:
/// `shell = "gitbash"` is Git's bash.exe and gets bash's integration.
pub(crate) fn detect_spawn_shell(
    shell_hint: Option<&str>,
) -> aterm_core::shell_integration::ShellType {
    spawn_shell_type(shell_hint, |hint| {
        aterm_pty::select_shell_with_origin(hint).0
    })
}

/// [`detect_spawn_shell`] over the resolver `select`: the integration is the
/// one for the program `select` names, never for the hint or a default beside it.
fn spawn_shell_type(
    shell_hint: Option<&str>,
    select: impl FnOnce(Option<&std::ffi::OsStr>) -> std::ffi::OsString,
) -> aterm_core::shell_integration::ShellType {
    let program = select(shell_hint.map(std::ffi::OsStr::new));
    aterm_core::shell_integration::ShellType::detect(&program.to_string_lossy())
}

/// The tab's own shell (Windows), handed to a fresh shell tab as
/// `ATERM_TAB_SHELL` so `aterm doctor` and `show-config` typed in it name the
/// shell the tab runs; no CLI process can see a window's launch flags. The
/// program is resolved by `select_shell_with_origin`, as the spawn itself does.
/// A `-e` session runs a command and gets none; the name is deny-listed, so an
/// inherited copy never stands in for it.
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

/// `COLORFGBG` for a Windows session whose palette reads as `appearance`
/// (`SessionFactory::shell_colorfgbg`; unknown is aterm's default, dark): rxvt's
/// `<fg>;<bg>` palette indices, `15;0` dark and `0;15` light, which vim's
/// `term_bg_default`, neovim and others read for `background`. Under the inbox
/// console host the OSC 11 query that would tell them never reaches aterm, so
/// Git Bash's vim drew its light-background palette on the dark theme. An
/// inherited copy describes the terminal aterm was started from and is
/// stripped (`aterm_pty`'s foreign-terminal list); this one is set after that.
#[cfg(any(windows, test))]
fn colorfgbg_env(appearance: Option<aterm_types::Appearance>) -> (String, String) {
    let value = match appearance {
        Some(aterm_types::Appearance::Light) => "0;15",
        Some(aterm_types::Appearance::Dark) | None => "15;0",
    };
    ("COLORFGBG".to_string(), value.to_string())
}

/// Prepare OSC 133/633 shell integration for the spawned shell: env additions,
/// an optional argv override (bash's `--rcfile`, carrying config `shell_args`
/// — `aterm_shell_integration::merge_shell_args`), and the raw capability nonce
/// for `Terminal::authorize_shell_integration`, so only this shell's marks are
/// trusted. `None` for an unknown shell, on I/O error, or for `shell_args` no
/// integrated start can honour; the shell still spawns without command-block
/// tracking (with those args, through the spawn's own `shell_args`), and the
/// outcome is recorded ([`ShellIntegrationOutcome`]). Runs in the parent, before
/// spawn.
fn prepare_shell_integration(
    shell_hint: Option<&str>,
    shell_args: Option<&[String]>,
) -> Option<ShellIntegrationSetup> {
    use aterm_core::shell_integration as si;
    let shell = detect_spawn_shell(shell_hint);
    let mut injection = match si::prepare(shell) {
        Ok(Some(injection)) => injection,
        Ok(None) => {
            // Name the shell the way the USER selected it, so the diagnosis
            // reads as their own configuration and not our detection guts.
            let name = selected_shell_name(shell_hint);
            // Said once per shell name per process; every tab still records it.
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

/// A fresh interactive shell's start, kept on its [`Session`]: when it was
/// spawned, the name it goes by (the basename of the program the spawn runs:
/// `zsh`, `fish`, `pwsh.exe`), and the input driven into its PTY at the spawn.
/// Only a shell gets one ([`records_shell_start`]), so a failed exit soon after
/// `at`, before the shell drew a prompt or anyone wrote to it, means the shell
/// itself failed to start (`App::shell_failed_at_start`).
#[derive(Clone, Debug)]
pub(crate) struct ShellStart {
    pub(crate) at: Instant,
    pub(crate) name: String,
    /// [`SinkWriter::driven_input`] before anything could write to the PTY. A
    /// key, a paste or a `send` since moves it, and a shell someone has typed
    /// into did not fail at start. aterm's replies to the shell's own queries
    /// (fish and powerlevel10k ask for DA, the cursor position and colours as
    /// they start) do not: they are no sign anyone used it.
    pub(crate) input: aterm_session::sink::DrivenInput,
}

/// Whether a spawn records a [`ShellStart`]: a fresh interactive shell does; a
/// `-e` command (its window closes when it exits, failure or not) and a shell
/// adopted across an update (started long before this process) do not.
fn records_shell_start(adopted: bool, exec_command: Option<&[std::ffi::OsString]>) -> bool {
    !adopted && exec_command.is_none_or(<[std::ffi::OsString]>::is_empty)
}

/// The shell a fresh spawn runs, named for a diagnosis: as the person selected
/// it (config `shell` / `--shell`), else the program the spawn resolves
/// (`$SHELL`, else the account's login shell on Unix).
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

/// The interactive shell a fresh spawn runs, resolved the way the spawn
/// resolves it: config `shell` / `--shell`, else `$SHELL`, else the
/// account's login shell (`aterm_pty::select_shell_with_origin`, the same
/// resolution on every platform's own terms; Windows: its default chain).
/// A worker's line is written for it (`task_launch::worker_dialect`).
pub(crate) fn interactive_shell_program(shell_override: Option<&str>) -> std::path::PathBuf {
    aterm_pty::select_shell_with_origin(shell_override.map(std::ffi::OsStr::new))
        .0
        .into()
}

/// The name [`interactive_shell_program`] goes by: its basename.
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
    /// `SHELL` for the child when the broken shell came from `$SHELL`, which
    /// the child would otherwise inherit.
    pub(crate) shell_env: Option<(String, String)>,
}

/// The substitute for a fresh interactive shell that cannot start: the shell
/// the spawn would select ([`aterm_pty::select_shell_with_origin`]) is checked
/// before the spawn ([`aterm_pty::shell_cannot_start`]: missing, a folder, not
/// executable, a bare name on no `PATH` entry), and when it cannot run, the
/// account's login shell, else `/bin/sh`, starts instead
/// ([`aterm_pty::fallback_shell`]) — VTE's and Terminal.app's answer to a broken
/// login shell. The config's `shell_args` were written for the shell that cannot
/// start, so the substitute runs without them (`bash` refuses fish's
/// `--no-config` and exits at once), and the line says so when there were any.
/// `None` when the shell can start, when nothing else can either (the spawn then
/// fails as it always did), and on Windows, whose seam selects from a chain of
/// its own.
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

/// The shell a fresh session starts and the config `shell_args` it is given: a
/// substitute's program alone (see [`substitute_shell`]), else the configured
/// shell and its args.
fn shell_to_start<'a>(
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

/// Write `sub`'s line into a fresh session's engine, on a line of its own. The
/// line names a program from the config or `$SHELL`, so its control characters
/// are dropped: it is text to read, never a sequence for the terminal.
fn say_substitute(term: &Mutex<Terminal>, sub: &ShellSubstitute) {
    let mut line: String = sub.notice.chars().filter(|c| !c.is_control()).collect();
    line.push_str("\r\n");
    term_lock(term).process(line.as_bytes());
}

#[cfg(all(test, unix))]
mod substitute_shell_tests {
    use super::{shell_to_start, substitute_shell};

    /// A shell that can start is not replaced; one that cannot is, by a program
    /// that can, and the line names the broken one, where it was set, and the
    /// substitute. (A bare `--shell bash` is no broken shell: it is looked up.)
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

    /// The broken shell's `shell_args` stay with it: the substitute starts
    /// without them, and the line says so. (`shell = "/nonexistent/fish"` with
    /// `shell_args = ["--no-config"]` started bash with `--no-config`, which
    /// printed its usage and exited, leaving no working shell.)
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
        // A shell that can start keeps its own args.
        assert_eq!(
            shell_to_start(None, Some("/bin/sh"), Some(&args)),
            (Some("/bin/sh"), Some(&args[..]))
        );
    }
}

#[cfg(test)]
mod records_shell_start_tests {
    use super::records_shell_start;

    /// Only a fresh interactive shell records a start: `-e false` and an
    /// adopted shell record none.
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

/// Everything `spawn_session` needs to stand up a new tab's shell session,
/// captured once at startup. The spawn/sandbox caps are the single root
/// authority minted in `main`, held by clone (cloning a `Cap` does not re-mint).
/// The baseline `env_add` is the terminal-identity env without the
/// shell-integration vars, which carry a per-tab nonce and are added per spawn.
/// `Clone` so a tab spawned off the event loop takes its own copy to the spawner
/// thread (`tab_spawn`).
#[derive(Clone)]
pub(crate) struct SessionFactory {
    pub(crate) spawn_cap: aterm_cap::Cap<aterm_cap::effects::Spawn>,
    pub(crate) sandbox_cap: aterm_cap::Cap<aterm_sandbox::Sandbox>,
    /// Terminal-identity env (TERM/COLORTERM/LANG/…) shared by every tab; the
    /// shell-integration vars, which embed a per-tab nonce, are appended per
    /// session in `spawn_session`.
    pub(crate) env_add: Vec<(String, String)>,
    /// `-e <cmd>`: run this instead of `$SHELL` (also disables shell integration),
    /// as the OS's argv gave it ([`crate::cli::Cli::exec_command`]).
    pub(crate) exec_command: Option<Vec<std::ffi::OsString>>,
    /// Config `shell` / `--shell`: the interactive shell every tab spawns
    /// (Windows: discovery-resolved — `"bash"` finds Git Bash off-PATH; Unix: a
    /// bare name looked up on PATH; absolute paths verbatim). `None` → the
    /// platform default. See `aterm_pty` shell selection.
    pub(crate) shell_override: Option<String>,
    /// Config `shell_args`: extra argv passed to `shell_override` (e.g. `-l -i`).
    pub(crate) shell_args: Option<Vec<String>>,
    /// `-d <dir>`: working directory for every tab's shell.
    pub(crate) cwd: Option<String>,
    /// OS-sandbox wrap (macOS Seatbelt SBPL). `Some(profile)` only in `Containment`
    /// mode on macOS: every tab's `spawn_shell` is wrapped in `sandbox-exec -p
    /// <profile>` to deny network (fail-closed if the wrapper is missing). `None`
    /// otherwise, an unwrapped spawn. Resolved once in `main` so all tabs match.
    pub(crate) sandbox_wrap: Option<String>,
    /// Engine config (scrollback/cursor/theme/palette) applied to each tab's
    /// `Terminal`, byte-identical to the single-session path.
    /// Its `color_scheme` is the palette's appearance
    /// (`ResolvedColors::reported`), so a tab spawned later reports the scheme
    /// its palette reads as (DEC 2031 / DSR `CSI ?996n`) from the same
    /// `apply_config` that lands its colours.
    pub(crate) terminal_config: Option<aterm_core::config::TerminalConfig>,
    /// Whether to inject OSC 133/633 shell integration. Each tab gets a fresh
    /// CSPRNG nonce, authorized and required on its own engine, so one tab's
    /// output cannot forge another's marks. False when `-e` runs a command or
    /// integration is opted out.
    pub(crate) integrate: bool,
    /// Latency epoch shared across tabs: each session's PTY reader stamps the
    /// leading edge of its output bursts (`Session::last_output_ns`) on this
    /// origin, so the present path's output-to-present subtraction is valid.
    pub(crate) lat_epoch: Instant,
    /// The event loop's turn state ([`LoopIdle`]), stamped by the main thread and
    /// read by every tab's PTY reader ([`gated_output_wake`]).
    pub(crate) loop_idle: Arc<LoopIdle>,
    /// Desktop-notification channel shared by every tab (bounded,
    /// `notify::NOTIFY_QUEUE_CAP`); the delivery thread (`notify::spawn_delivery`)
    /// owns the receiver.
    pub(crate) notify_tx: std::sync::mpsc::SyncSender<notify::NotifyMsg>,
    /// Config `allow_kitty_file_transfer` (default false): each tab installs the
    /// Kitty non-direct-medium resolver so `t=f`/`t=t`/`t=s` images load from host
    /// files or shared memory. Off: those mediums are skipped.
    pub(crate) allow_kitty_file_transfer: bool,
    /// Config `temporal_recording` (default false): each tab runs the temporal
    /// recorder (keyframe seed, writer thread, and the reader's `RawIn`/`Reply`
    /// taps, read back by the `temporal` verb). Off: none of that is wired;
    /// `SessionCtx.temporal` still exists as an empty recorder.
    pub(crate) temporal_recording: bool,
    /// Overlap handoff (incoming side): when true, adopted sessions are built
    /// without their PTY reader; [`attach_reader`] runs after every carried
    /// window has presented and the readiness byte told the parked parent to
    /// exit (`App::maybe_signal_handoff_ready`). Until then no bytes are
    /// consumed, so a child that dies pre-ready leaves them for the parent.
    /// Set iff `ATERM_HANDOFF_READY_FD` arrived; fresh spawns attach inline.
    pub(crate) defer_adopted_readers: bool,
    /// A tab spawned off the event loop (`tab_spawn`): `spawn_session` leaves
    /// the reader to the caller, which parks it on a gate the main thread opens
    /// once the tab is installed and registered, so the reader's first
    /// `Wake::Ready` or `Wake::Exit` never reaches the loop before the session
    /// it names. Set only on the spawner's copy of the factory.
    pub(crate) reader_after_install: bool,
    /// The foreground-process-group probe each PTY gather samples for the
    /// foreground handback, and the status sweep reads for the group in front
    /// (`App::observe_session_statuses`): [`crate::quit_safety::foreground_pgrp`]
    /// (`tcgetpgrp(master)`, `-1` when unknown). A plain `fn` so the gather thread
    /// copies it; `foreground_handback_conformance` swaps in a scripted probe.
    pub(crate) fg_probe: fn(i32) -> i32,
    /// Whether a foreground group is gone, the liveness half of the foreground
    /// handback (a stopped job, or `gdb -tui` handing the terminal to its
    /// inferior, still runs and keeps its modes). Shipping is
    /// [`crate::foreground_handback::group_gone`]. Called as
    /// `fg_gone(master, pgid, role)`, only outside the term lock
    /// ([`crate::foreground_handback::FgOwners::probe_edge`]).
    pub(crate) fg_gone: crate::foreground_handback::FgGone,
    /// Whether the shell `pid` runs a program it started at or after a
    /// wall-clock instant (unix epoch ms) where the foreground group cannot
    /// show it: in the shell's own group on unix (PowerShell keeps every
    /// program there), any child under ConPTY. Shipping is
    /// [`crate::quit_safety::shell_runs_program_since`]; each reader installs
    /// it on its engine (`Terminal::set_program_probe`), which asks it only
    /// for a cwd report no one signed at the shell's prompt and while one it
    /// let land shows. A plain `fn`, so a test scripts its own.
    pub(crate) program_probe: fn(i32, u64) -> bool,
    /// The kernel's name for the leader of foreground group `pgid` when no other
    /// process is in its group, read when a file citation asks whether a shell in
    /// front is the one a person types into (`path_link::runs_here`). Shipping is
    /// [`crate::path_link::lone_leader`] (`aterm_sysprobe::lone_group_leader`); a
    /// plain `fn`, so a test whose groups lead no process puts in its own.
    pub(crate) lone_leader: fn(i32) -> Option<String>,
    /// Whether the program leading foreground group `pgid` has handed the
    /// terminal to another program in its group, asked when a bare
    /// Ctrl-R/Ctrl-S decides whether an agent's composer owns it
    /// (`App::front_agent_composer`). Shipping is
    /// [`crate::app_input::agent_handed_terminal`]
    /// (`aterm_sysprobe::stdin_sharers`); a plain `fn`, so the test App answers
    /// `false` and a test scripts its own.
    pub(crate) handed_terminal: fn(i32) -> bool,
    /// The image of foreground group `pgid`'s leader (card engine §3.4), read by
    /// each PTY gather at a foreground edge (given no last image) and by the
    /// parse stage's byte arm (given the group's last, which the read keeps for
    /// an unchanged process): shipping is [`crate::holder_image::read`] (the
    /// kernel's executable, start and, for a script shell, first argument); the
    /// test App carries the inert [`crate::holder_image::unread`], and a test
    /// scripts its own.
    pub(crate) edge_image: crate::holder_image::ReadImage,
    /// THE EDGED INBOX (card engine §3.5): every session's parse stage pushes
    /// its id once per published holder edge, and the status sweep visits those
    /// first (`App::observe_session_statuses`). One per App: every copy of this
    /// factory shares it.
    pub(crate) edge_inbox: crate::holder_image::EdgeInbox,
    /// Whether each PTY reader runs the byte-driven arm (card engine §3.6): the
    /// initial holder observed once, unresolved signed-C shell/Launcher once
    /// per batch, and a read after each accepted C boundary. Settled agent
    /// output adds no image reads. Every unix; a reader whose
    /// [`Self::proc_watch`] answered runs its initial read alone, since the
    /// kernel's `exec` edges order every later one.
    pub(crate) byte_arm: bool,
    /// Each PTY reader's process watch on its shell (card engine §3.3), made
    /// at attach: shipping is [`crate::exec_edges::shipping_watch`] (Darwin's
    /// kqueue; `None` elsewhere and on `EMFILE`, which the byte-driven arm
    /// then covers in full); the test App's is inert, and a test scripts one
    /// ([`aterm_uds::procwatch::ProcWatch::scripted`]).
    #[cfg(unix)]
    pub(crate) proc_watch: fn(i32) -> Option<aterm_uds::procwatch::ProcWatch>,
    /// Whether each PTY reader runs the dev `card-seen` probe
    /// ([`crate::holder_image::card_probe_requested`]): a `DEV_ROWS` build run
    /// for a lead measurement. Off, a dev build's term-lock holds are the
    /// product's, so its timing runs measure what ships.
    pub(crate) card_probe: bool,
    /// Overlap handoff (incoming side), armed like
    /// [`Self::defer_adopted_readers`]: re-key files of adopted shells are held
    /// here until the update's Commit (`shell_rekey::Deferred`), released after
    /// the reader gate opens. `None` otherwise: the adoption is final and writes
    /// at once.
    pub(crate) rekey_deferred: Option<Arc<crate::shell_rekey::Deferred>>,
    /// A warm spare's spawn (`crate::spare`), set only on the `aterm-spare`
    /// thread's factory copy, like [`Self::reader_after_install`]: the shell is
    /// spawned with its hold FIFO, its host effects only taint it, its reader
    /// attaches live through the spare's own post (which swallows every wake
    /// until adoption), and the primer and the keeper wait for the adoption.
    pub(crate) spare: Option<crate::spare::SpareSpawn>,
    /// The light/dark side of the theme this spawn's session will draw, where
    /// it draws one of its own (`App::spawn_shell_appearance`): what its shell's
    /// `COLORFGBG` names ([`SessionFactory::shell_colorfgbg`]). `None` names the
    /// instance palette's side (`terminal_config.color_scheme`). Set only on a
    /// spawn's copy of the factory, at booking: the session's theme itself lands
    /// after its shell has started.
    #[cfg(windows)]
    pub(crate) shell_appearance: Option<aterm_types::Appearance>,
}

impl SessionFactory {
    /// What a spare spawned by this factory shares with a cold tab spawned by
    /// it now (`crate::spare`'s gate G4): every field that shapes the shell or
    /// its session, and the process environment. Not the colours (a parked
    /// spare catches up at landing; a warm one is checked apart). Exhaustive
    /// destructuring: a new field does not compile until it is classified here.
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
            // Hashed as the `COLORFGBG` it names, below.
            #[cfg(windows)]
                shell_appearance: _,
        } = self;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        "aterm-spawn-fingerprint-v1".hash(&mut h);
        // The side a Windows shell is told its terminal is on: a spare started
        // under the other side would tell its programs the wrong background.
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
        // The process environment every shell inherits (the control dir a
        // shell is told derives from it: `$XDG_RUNTIME_DIR`, else `$HOME`).
        // It is fixed once `main`'s prologue is done (a `set_var` beside
        // another thread is a data race, so nothing moves it while a spare can
        // exist): hashed once, at first use, so no tick or Cmd-T on the main
        // thread walks it, sorts it or stats a folder for it.
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

    /// The `COLORFGBG` this factory's shell starts with ([`colorfgbg_env`]): the
    /// side of its session's own theme ([`Self::shell_appearance`]), else the
    /// instance palette's. A shell already running keeps the value it started
    /// with when its theme or the OS appearance later moves.
    #[cfg(windows)]
    pub(crate) fn shell_colorfgbg(&self) -> (String, String) {
        colorfgbg_env(
            self.shell_appearance
                .or_else(|| self.terminal_config.as_ref().and_then(|tc| tc.color_scheme)),
        )
    }
}

/// The commit latch a new session holds: the incoming overlap handoff's Commit
/// ([`SessionFactory::rekey_deferred`]) for a session adopted through it, so
/// dropping that session before Commit signals nothing the parked predecessor
/// still owns (`Session::drop`). `None` for a fresh spawn, a shell the PTY
/// keeper kept, and every boot without an overlap handoff.
pub(crate) fn adopted_handoff_commit(
    adopted: bool,
    recovered: bool,
    rekey_deferred: Option<&Arc<crate::shell_rekey::Deferred>>,
) -> Option<Arc<crate::shell_rekey::Deferred>> {
    rekey_deferred.filter(|_| adopted && !recovered).cloned()
}

/// Whether `s` is a well-formed session id (`s-` + 20 lower-case hex chars / 80
/// bits), the shape [`SessionId::generate`] produces. Validates an injected id
/// before adoption, so a malformed `ATERM_SESSION_ID` falls back to a fresh
/// identity, and an adopted id is one the wire's selectors can name
/// ([`aterm_types::control_request::Sid`]).
pub(crate) fn is_valid_session_id(s: &str) -> bool {
    s.len() == 22 && aterm_types::control_request::Sid::parse(s).is_some()
}

/// Parse an injected root identity from the recursion env values. Fail-closed:
/// adopts only when both a well-formed session id and a parseable nonce are
/// present; otherwise `None` and the caller generates a fresh identity.
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

/// Read and claim this aterm's injected root identity (set by an outer aterm
/// that spawned us). `None` (fresh identity) when unset, malformed, or held by a
/// live instance. Only the root session (`id == 0`) adopts it, so the outer's
/// preminted edges authorize against our table.
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

/// The adoption decision, against a caller-named control directory.
///
/// `$ATERM_SESSION_ID` is re-readable so a child relaunched in the same shell
/// keeps its identity and the outer's edges. Adoption requires the identity to
/// be provably unheld ([`crate::identity_claim::claim_for_adoption`]), so two
/// live instances never answer to one id; a launch that cannot prove it mints a
/// fresh id (and says so on stderr). `None` with no control directory: nowhere
/// to contend is nowhere to prove.
///
/// An adopted id's predecessor may have run a moment ago: once the claim is
/// held, `epochs` (the process's allocator) notes the epoch its holders
/// recorded for the id's log (`identity_claim::recorded_epoch_in`, module doc
/// "The epoch record"), so this process's log of it, which starts after this,
/// takes one above it whatever the wall clock says.
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

    /// Two launches from one shell read the same `$ATERM_SESSION_ID`: the first
    /// adopts, the second is refused and mints a fresh id.
    #[test]
    fn two_launches_from_one_shell_do_not_share_an_identity() {
        // The claims are parked process-wide, and `release_claims_for_test` (here
        // and in `identity_claim`'s tests) drops them all: serialize with it.
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

        // The first launch is still live (its claim parked) when the second reads
        // the same export; no clock enters the decision.
        assert!(
            adopt_injected_identity_in(Some(dir.path()), Some(&sid), Some(&hex), &Epochs::new())
                .is_none(),
            "a second launch from the same shell must NOT answer to a live id"
        );

        // Negative control: once the first launch releases its claim, the same
        // arguments adopt again (a relaunch).
        crate::identity_claim::release_claims_for_test();
        let relaunch =
            adopt_injected_identity_in(Some(dir.path()), Some(&sid), Some(&hex), &Epochs::new())
                .expect("a relaunch after the first launch exits adopts the premint");
        assert_eq!(relaunch.0, premint);
        crate::identity_claim::release_claims_for_test();
    }

    /// Fail-closed both ways: a half-injected identity is refused before any
    /// claim, and a well-formed one with no control directory is refused too.
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

    /// THE EPOCH RECORD (`identity_claim`'s module doc, the review of 6A-2): a
    /// relaunch from one shell under the premint's id, inside the wall second
    /// its predecessor's log started in (a hand-set stamp, so no clock enters),
    /// takes an epoch above the predecessor's, because the predecessor
    /// recorded its log's epoch beside the claim and the relaunch notes it once
    /// its claim is held. A handoff successor's larger epoch raises the record
    /// and the next relaunch goes above that; a smaller one never lowers it; a
    /// successor of an id with no record writes none; and a record no producer
    /// writes (above the ceiling, or not a number) reads as none.
    /// NEGATIVE CONTROL: a process told nothing (a build before the record)
    /// takes the predecessor's epoch again; and scratch-dropping the note from
    /// the adoption repeats it in the test's own relaunch.
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
        // One launch from the shell: it adopts, its log starts, and its epoch
        // is recorded (as `spawn` does once the log's epoch is final); then
        // it exits (its claim released).
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
        // A seamless update's successor of the relaunch holds its epoch plus
        // one (a carried successor's) and raises the record; a smaller one
        // never lowers it.
        crate::identity_claim::record_epoch_in(dir.path(), &premint, second + 1, false);
        crate::identity_claim::record_epoch_in(dir.path(), &premint, first, false);
        assert_eq!(recorded(), Some(second + 1));
        let third = launch();
        assert!(third > second + 1, "{second} + 1 then {third}");
        // An id handed on with no record (a minted one) gets none.
        let minted = SessionId::generate();
        crate::identity_claim::record_epoch_in(dir.path(), &minted, 7, false);
        assert_eq!(
            crate::identity_claim::recorded_epoch_in(dir.path(), &minted),
            None
        );
        // A record no producer writes reads as none, and a launch rewrites it.
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
            // The former predictable stage is an ordinary preexisting alias.
            // Neither the new epoch writer nor the older marker writer may
            // open it, truncate its target, or clean it up as their own.
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

            // Final-file aliases are not epoch authority, even with decimal
            // contents. Only our own test entries are removed between shapes.
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

/// The identity and capability tokens a parent mints for ONE child: the edges
/// the child installs from its `ATERM_EDGE_TOKENS` file. No forward presents
/// them: the parent reaches the child with the child's own token.
#[derive(Clone)]
pub(crate) struct ChildProvision {
    pub(crate) child_sid: SessionId,
    pub(crate) child_nonce: LaunchNonce,
    pub(crate) read: EdgeToken,
    pub(crate) write: EdgeToken,
    pub(crate) signal: EdgeToken,
}

/// The child's view of who spawned it: identity only, no capability.
///
/// `ENV_PARENT_SESSION_ID` names the hosting session so a child can address it
/// as `@self`; the edges carry authority and stay behind the recursion gate.
/// Every session gets it, including a one-shot `-e <cmd>` session hosting an
/// agent CLI whose hooks need their pane. Deny-listed, so an inherited copy
/// never survives a hop.
pub(crate) fn provision_child_identity_env(parent_sid: &SessionId) -> Vec<(String, String)> {
    use aterm_types::domain::ENV_PARENT_SESSION_ID;
    vec![
        (
            ENV_PARENT_SESSION_ID.to_string(),
            parent_sid.as_str().to_string(),
        ),
        // The multiplexer baseline: `aterm ctl` detects a multiplexer pane by
        // comparing this birth signature with the live one. Stamped here, not by
        // the shell integration, so a shell that never sources it does not
        // inherit a pane's stamp and get refused as a pane. Deny-listed.
        (
            aterm_types::domain::ENV_MUX_BASE.to_string(),
            mux_env_signature_now(),
        ),
    ]
}

/// This process's multiplexer signature at the moment a session is born:
/// `"<$TMUX>|<$STY>"`, empty halves and all. The raw pair, not a boolean, so
/// entering a different multiplexer later reads as a crossing too.
fn mux_env_signature_now() -> String {
    let get = |k: &str| std::env::var(k).unwrap_or_default();
    format!("{}|{}", get("TMUX"), get("STY"))
}

/// The controller-spawn observation hint: `ATERM_OBSERVE_SESSION_ID=<origin sid>`,
/// telling the newborn supervisor which session it holds a connection over.
/// Identity only, never a token (authority lives in the origin's edge table).
/// Deny-listed (`env_sanitize.rs`), so a grandchild does not inherit it.
pub(crate) fn provision_observe_env(origin_sid: &SessionId) -> Vec<(String, String)> {
    use aterm_types::domain::ENV_OBSERVE_SESSION_ID;
    vec![(
        ENV_OBSERVE_SESSION_ID.to_string(),
        origin_sid.as_str().to_string(),
    )]
}

/// Mint a fresh child identity and the three per-op capability edges
/// (read/write/signal) this session grants over the child it is about to host.
/// Returns the env pairs for the child and the [`ChildProvision`] the parent
/// keeps. The inner aterm adopts the identity and inserts the edges
/// ([`register_injected_parent_edges`]), so no manual `grant` is needed; all
/// three ops are required or recursion would be read-only.
pub(crate) fn provision_child_recursion_env() -> (Vec<(String, String)>, ChildProvision) {
    use aterm_types::domain::{ENV_LAUNCH_NONCE, ENV_SESSION_ID};
    let prov = ChildProvision {
        child_sid: SessionId::generate(),
        child_nonce: LaunchNonce::generate(),
        read: EdgeToken::generate(),
        write: EdgeToken::generate(),
        signal: EdgeToken::generate(),
    };
    // Adoption identity (non-secret). The parent id is set unconditionally by
    // `provision_child_identity_env`. The edge-token secrets are not in env; the
    // caller routes them through a 0600 file ([`append_edge_token_channel`]).
    let env = vec![
        (
            ENV_SESSION_ID.to_string(),
            prov.child_sid.as_str().to_string(),
        ),
        (ENV_LAUNCH_NONCE.to_string(), prov.child_nonce.to_hex()),
    ];
    (env, prov)
}

/// Route the parent-to-child edge-token secrets through a 0600 file: write them
/// to `<sock_dir>/edges/<child-sid>` and append only that path to `env`.
///
/// With no private socket dir, or when the file cannot be written, nothing is
/// appended: the child installs no parent edges (fail-closed). The tokens never
/// enter the inheritable environment
/// (`aterm_models_effects::capability_secrecy_model`). A dir that refuses the
/// file is logged.
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

/// The whole recursion provisioning for one child: its adopted identity (env)
/// and its edge secrets through the 0600 file ([`append_edge_token_channel`]).
///
/// The returned [`proxy::ChildProxy`] owns the file and removes it when
/// dropped: at the session's close, or when a spawn fails after provisioning.
///
/// The identity and its file come ready from [`crate::spawn_prep`] when it holds
/// one written under `sock_dir`: minted and written ahead, so the spawn waits for
/// no file create.
pub(crate) fn provision_child_proxy(
    sock_dir: Option<&Path>,
) -> (Vec<(String, String)>, proxy::ChildProxy) {
    let (env, _, edge_file) = crate::spawn_prep::take_provision(sock_dir)
        .unwrap_or_else(|| provision_child_files(sock_dir));
    (env, proxy::ChildProxy::new(edge_file))
}

/// [`provision_child_proxy`] for an identity and tokens already minted
/// ([`provision_child_recursion_env`]), so a test can know the secrets it
/// routes.
#[cfg(test)]
fn provision_child_proxy_from(
    (mut env, prov): (Vec<(String, String)>, ChildProvision),
    sock_dir: Option<&Path>,
) -> (Vec<(String, String)>, proxy::ChildProxy) {
    let edge_file = append_edge_token_channel(&mut env, &prov, sock_dir);
    (env, proxy::ChildProxy::new(edge_file))
}

/// The files half of [`provision_child_proxy`]: the child's identity env and
/// tokens ([`provision_child_recursion_env`]) and its
/// edge-token file ([`append_edge_token_channel`]).
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

/// Insert the parent-preminted edges into a child-side [`EdgeTable`] from the
/// injected env values, binding each to the child's `self_id` (dst) and
/// `nonce`. Returns the number recorded. Missing or garbled values are skipped
/// per token; a missing parent id records nothing.
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

/// Record the parent's preminted edges (from this process's injected env) into
/// the root session's edge table, so the outer aterm that spawned us holds the
/// authority it granted. Only meaningful for the adopted root session.
fn register_injected_parent_edges(ctx: &SessionCtx) {
    use aterm_types::domain::{ENV_EDGE_TOKENS, ENV_PARENT_SESSION_ID};
    let parent = std::env::var(ENV_PARENT_SESSION_ID).ok();
    if parent.is_none() {
        return;
    }
    // The secrets arrive only through the 0600 file named by `ATERM_EDGE_TOKENS`.
    // The read is non-destructive: a child relaunched in the same shell re-reads
    // the same file. The parent removes it (its `proxy::ChildProxy`). No path
    // means no parent edges.
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
    // The parent always mints all three ops, so fewer than three recorded means a
    // malformed, duplicate or partial token set lost some authority: say so.
    if n < 3 {
        crate::logging::stderr_line!(
            "aterm: ATERM_PARENT_SESSION_ID set but recorded only {n}/3 parent edges — \
             some ops have no authority (malformed/duplicate/partial edge tokens)"
        );
    }
}

/// A PTY master handed across a seamless re-exec and not yet adopted. It owns
/// the descriptor: dropped un-adopted it closes it, so a handed-off master is
/// always either adopted ([`HandedMaster::into_sink`]) or closed
/// (`aterm_models_session::fd_handoff_no_leak_model`). An open un-adopted master
/// would be a readerless PTY whose shell blocks once the buffer fills.
///
/// A number below 3 is never closed: no handoff names one, and `-1` is the
/// test fixtures' sentinel.
pub(crate) struct HandedMaster(i32);

impl HandedMaster {
    /// Take ownership of handed-off master `fd`.
    pub(crate) fn new(fd: i32) -> Self {
        Self(fd)
    }

    /// The descriptor, still owned by this handle.
    pub(crate) fn raw(&self) -> i32 {
        self.0
    }

    /// Adopt the master: move it into its session byte sink ([`master_sink`]),
    /// re-arming `FD_CLOEXEC` first (the outgoing process cleared it to survive
    /// the exec; left clear it would leak into every later subprocess).
    ///
    /// Shared by [`spawn_session`] and the headless harness's stand-in, so a
    /// conformance adopting through the stand-in drives the shipping path.
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

/// The one byte sink for PTY master `master` (whole-frame atomicity across the
/// keyboard path, control writer verbs and the reader's query replies), and
/// the master's sole owner from here on: the descriptor closes only when the
/// last `Arc<SinkWriter>` clone drops, so it is never closed under a parked
/// reader or in-flight writer, nor recycled by a later forkpty. `Session::drop`
/// does not close it.
///
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

/// A live session handed across a seamless-update re-exec: its restored fabric
/// identity and the PTY master fd/pid the new process re-adopts. The outgoing
/// process cleared `FD_CLOEXEC` on `master` so the same shell keeps running;
/// [`spawn_session`] with `adopt: Some(_)` attaches an engine and reader to it.
/// The shell keeps its original env, so integration and recursion tokens are
/// not re-minted.
pub(crate) struct Adopted {
    /// The outgoing session's pool id, so the incoming boot places this shell
    /// back into its original pane: the restore manifest's matching leaf records
    /// the same id.
    pub local_id: u64,
    /// The PTY master fd inherited across the exec (CLOEXEC was cleared before it),
    /// owned until [`spawn_session`] moves it into the session's sink: an `Adopted`
    /// that is dropped instead closes it (see [`HandedMaster`]).
    pub master: HandedMaster,
    /// The (still-running) child shell's pid == pgid.
    pub pid: i32,
    /// The session's restored stable fabric identity (from the handoff manifest).
    pub sid: SessionId,
    /// The session's restored launch nonce.
    pub nonce: LaunchNonce,
    /// Screen carry: the outgoing engine's checkpoint, hydrated before the
    /// reader starts so the window shows the pre-update screen. It holds a
    /// bounded scrollback tail (`seamless::max_handoff_history_lines`); older
    /// scrollback rides [`Self::history`] and is imported after Commit. `None`
    /// only for non-handoff construction.
    pub checkpoint: Option<aterm_core::terminal::TerminalCheckpoint>,
    /// Control carry (`crate::handoff_carry`): the outgoing session's turn ledger
    /// and alt-screen archive tail, when carried and valid. `None` adopts without.
    pub control: Option<crate::handoff_carry::ControlCarry>,
    /// `checkpoint` is not the program's last screen: the producer carried a
    /// blank one (`ScreenCarry::repaint`), or `seamless::take_incoming` refused
    /// the carried screen and degraded this session to a blank engine.
    /// [`spawn_session`] then pulses the PTY size so the program redraws.
    pub repaint: bool,
    /// This shell was spawned by a build before self-healing sessions
    /// (`session_store::predates_path_self_heal`): its PATH lacks `<prefix>/agents/`.
    /// Decided by `seamless::take_incoming` from the manifest's `outgoing_build`
    /// (absent means predates) or the record's `frozen_path`; marked on the
    /// registry at `App::register_session` and carried by the next handoff.
    pub frozen_path: bool,
    /// Broadcast opt-ins: the record's `topic add` set as `"<topic> <since>"`
    /// rows, already validated, re-seeded onto the adopted `SessionFabric` so a
    /// seamless update does not unsubscribe an agent from its topics.
    pub topics: Vec<String>,
    /// The process group the outgoing reader last saw holding the terminal
    /// (`SessionRecord::fg_holder`). Seeds `Session::fg_holder`, so a job that
    /// died during the handoff is an edge at the first sample. `0`: none carried.
    pub fg_holder: i32,
    /// The record says this shell reads a re-key file at every prompt
    /// (`shell_rekey`); an adopted session without its nonce is re-keyed through it.
    pub rekey: bool,
    /// The record says this shell runs an integration loader that reads a body
    /// pointer at every prompt (`shell_body`). A session with one, or whose
    /// checkpoint carried a signed revision, is pointed at this build's body; one
    /// with neither runs a pre-loader script (`integration_rev=frozen`).
    pub loader: bool,
    /// History carry (`crate::handoff_history`): the sidecar of scrollback older
    /// than `checkpoint`, opened before the proof and imported after Commit, plus
    /// the lines this handoff could not carry. Default for fresh construction.
    pub history: crate::handoff_history::AdoptedHistory,
    /// The handoff record's `"<origin> <reason>"` hold rows, unparsed, one per
    /// standing origin (`SessionRecord::hold_rows`, or a keeper META's);
    /// [`seed_adopted_fabric`] reads them fail-closed (`fabric::parse_holds`)
    /// before registration, so a seamless update never lifts a halt — the
    /// owner's or the fleet's. Empty: not held.
    pub holds: Vec<String>,
    /// The fallback title for a checkpoint that carried no title of its own:
    /// the engine title the layout leaf naming this session was captured with
    /// at the park, or the handoff record's copy (`SessionRecord::title`),
    /// which is fresh only for the pane in front of its window — taken with
    /// no placed layout, and for that pane when its leaf reads as a park read
    /// that found the engine busy (no cwd, no title) (round eight of the
    /// update audit, U47; `seamless::take_incoming`).
    pub title: String,
    /// Another supervisor's lease, the record's `"<holder> <remaining_ms>"` row,
    /// unparsed; `session_timeline::seed_carried_claims` restores it before
    /// registration. `None`: no lease held.
    pub supervisor: Option<String>,
    /// The record's keyed escalations (`"<owner> <pct-text>"` attention rows),
    /// unparsed, seeded beside the lease.
    pub attention_owners: Vec<String>,
    /// Whether this build's in-GUI host holds off the session for its
    /// adopted-claim grace (`SessionRecord::claim_grace`): the producer did
    /// not vouch that nobody else held it. `main_entry` hands the host these
    /// sessions' sids before its Commit resume.
    pub claim_grace: bool,
    /// The build that handed this shell across (the manifest's `outgoing_build`;
    /// `None` from a producer that wrote none), named by the handoff gap each of
    /// the session's recorders opens with ([`mark_handoff_gap`]).
    pub outgoing_build: Option<u64>,
    /// The PTY keeper's copy: `Some` for a master a crashed window left in the
    /// keeper's custody, with its release obligation
    /// ([`crate::keeper_link::Claim`]). [`spawn_session`] hands it to
    /// `keeper_link::register`; dropped unregistered, it releases the keeper's
    /// copy. `None` for an update's handoff.
    pub keeper: Option<crate::keeper_link::Claim>,
    /// THE RENDER DEBT (R08): what the outgoing engine owed the alternate
    /// screen this session restores onto (rows moved, drawn columns cut)
    /// (`SessionRecord::render_debt`), only when non-zero and the screen was
    /// carried. [`spawn_session`] seeds the adopting engine with it
    /// (`Terminal::seed_render_debt`) and books the judgement of it from
    /// attach, so the epoch judge and the repaint nudge treat the tab exactly
    /// as they would have in the parent. `None`: owes nothing, or the producer
    /// predates the field, and nothing happens.
    pub render_debt: Option<aterm_core::terminal::RenderDebt>,
    /// THE MAILBOX THE HANDOFF CARRIED (round seven of the update audit,
    /// findings 6, 8 and 28): the record's posts still waiting, receipts still
    /// owed and id counters, put back by [`seed_adopted_fabric`]. Empty for
    /// every fresh construction and from a producer that predates the carry.
    pub fabric: crate::fabric::FabricCarry,
    /// The record's newest timeline id: the adopted timeline counts on above it
    /// ([`mark_handoff_gap`]). `0`: none carried.
    pub timeline_id: u64,
    /// The session log's epoch, node-id counters and seq count the record
    /// carried (`SessionRecord::log_ids`, `SessionRecord::log_seq`): the
    /// adopted log continues them ([`mark_handoff_gap`]). Default: none
    /// carried (an older producer, a keeper's shell).
    pub log: crate::session_log::LogAdopt,
    /// The pair the crash journal image this window takes over at its Commit
    /// keys this shell by, when the outgoing process held an image that was not
    /// its own (`SessionRecord::journal_key`). Named by this window's keeper
    /// registration and its own images ([`crate::keeper_link::Tag::keyed`]).
    /// `None` for everything else.
    pub journal_key: Option<(u32, u64)>,
    /// The record's task row (`SessionRecord::task`), unparsed;
    /// `task_record::seed_carried` reads it before registration, so a worker
    /// keeps its task record — rev, state and since — across the update.
    /// `None`: no task, or a producer that predates the carry.
    pub task: Option<String>,
    /// The record's Autopilot end row (`SessionRecord::autopilot_end`),
    /// unparsed: [`seed_adopted_end`] puts its face's wall time on the
    /// timeline before registration, so the loop's restated face keeps it.
    /// `None`: no end and no work carried, or a producer that predates it.
    pub autopilot_end: Option<String>,
}

/// A SEAMLESS UPDATE'S CARRIED END onto an adopted session's timeline,
/// before the session is registered (`docs/DESIGN-harness-autopilot-
/// 2026-10-15.md` §11.2.3, item 8): the face the outgoing timeline held for
/// the end, `landed` (`CarriedEnd::told`), and the wall time it moved there,
/// held as a face held over "unknown" is
/// (`SessionTimeline::seed_task_face_held`). The loop's start restates the
/// face from "unknown" — whenever it comes, before or after the App's first
/// publication spends the rest of the row (`App::adopt_end_carry`) — and
/// takes that time back through `set_task_face`'s rule, so the chip, its
/// title, the card's line and the page's state line read `landed 17:42`, not
/// the time the successor's loop came back (§9.4.8). A restated face that
/// is another takes its own time, as ever. Nothing for a row that does not
/// read, a row with no end, or none (a fresh spawn).
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

/// Mark an adopted session's timeline, cast and temporal recorder as starting
/// at the handoff ([`crate::session_timeline::HandoffGap`]), since none of them
/// crosses the update: the timeline's `handoff carried=0 from_build=<n>` row,
/// the cast header's `aterm_handoff`, `temporal status`'s tail, and the session
/// log's `gap why=handoff epoch=<ep+1>` (`gap why=crash` for a shell the PTY
/// keeper held, `keeper`). Called before any other row reaches the timeline.
///
/// Timeline ids continue above `timeline_id`, the last one the outgoing process
/// issued, so every row is above any `timeline since=` anchor taken before; the
/// session log's node ids continue the counters `log` carried. A keeper's
/// recovery carries no count (`0`), and an anchor above the count it starts
/// reads its rows from the first (`SessionTimeline::since`).
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

/// Restore the broadcast opt-ins and standing halt a seamless update carried
/// onto the adopting session's `SessionFabric`. Called by [`spawn_session`]
/// before `App::register_session`, so an adopted shell never answers a `send`
/// it was halted against.
///
/// Topic rows are re-validated (`fabric::parse_topics` drops what does not
/// read); the hold rows fail closed (`fabric::parse_holds`), each into the
/// slot of the origin it reads as.
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
    // THE MAILBOX (round seven, findings 6/8/28): the posts a sender was told
    // are queued, the receipts still owed, and the id counters the sid's
    // bridge pins are keyed by — each id this session hands out from here is
    // above the old process's.
    fabric.seed_carry(carry, timeline);
}

/// Authorize the adopted shell's nonce on the successor engine after
/// `restore_checkpoint` restored `require_shell_integration_nonce`. The shell
/// keeps signing OSC 133/633 marks with its spawn nonce (carried by
/// `checkpoint_carry`). A parent that carried none leaves the requirement
/// standing (clearing it would let output forge marks); `status
/// integration=degraded` reports it.
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
///
/// This happens only through the consumer's degrade
/// (`seamless::admit_incoming_screen`). A fresh engine defaults the requirement
/// off, which would let output forge OSC 133/633 marks; the shell signs with a
/// nonce this process cannot know, so the requirement is set and the session
/// reports `integration=degraded`, as [`authorize_adopted_shell_nonce`] does.
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

/// Dev seam `$ATERM_DEBUG_LOST_SHELL_NONCE` (`aterm_types::dev_seam!`): spawn a
/// fresh shell that signs marks with a nonce its engine never learned and has
/// no re-key channel, so the typed re-key (`shell_rekey::typed`) can be driven
/// end to end without a real handoff.
fn debug_lost_shell_nonce() -> bool {
    aterm_types::dev_seam!("ATERM_DEBUG_LOST_SHELL_NONCE")
        .is_some_and(|v| !v.is_empty() && v != "0")
}

/// Restore an adopted session's engine before its reader starts; the only place
/// [`spawn_session`] does so.
///
/// 1. Screen carry: `checkpoint` is restored (the exact pre-update screen), its
///    carried nonce authorized ([`authorize_adopted_shell_nonce`]), then the
///    control carry's archive installed. With no checkpoint the mark
///    requirement is held ([`hold_adopted_shell_nonce_requirement`]) and the
///    archive is installed all the same (its rows are not about the screen), but
///    without the differ's state, which describes a screen this engine does not
///    show.
/// 2. Re-key (`shell_rekey`): if step 1 left the engine `Degraded` (no nonce
///    crossed) and the shell has a channel file (`rekey`), a fresh nonce is
///    written to it with no engine lock held, then authorized as a key the shell
///    has not taken yet (`status integration=` reads `on` from its first signed
///    mark, not from the write). No reader is attached, so no mark lands between
///    write and authorization. Under an overlap handoff (`deferred`) the file is
///    written only after the update's Commit, so an abandoned attempt never
///    moves a shell onto a key only the abandoned candidate knew; nobody holds
///    a key before its file is written, so authorizing it early lets no mark in.
/// 3. History carry (`crate::handoff_history`): `history`'s lines are imported
///    after Commit, in front of the restored history, so their absolute-row keys
///    are reserved now, under the restore's guard, and the import moves no row's
///    key. The reserve's claim fences the import (a scrollback the shell clears
///    first stays cleared), and the restore's bottom is noted
///    ([`crate::handoff_history::AdoptedHistory::anchor_in`]) so the viewport the
///    import puts back is measured from the park's bottom.
///
/// The lock-order census reads lock lifetimes by brace depth: step 1's guard is
/// scoped to its block, and step 2 locks again only after it closes.
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

/// The session log an adopted session continues: the counts its manifest
/// record carried (`Adopted::log`: epoch, node ids, seq, the keeper's epoch)
/// with the carried log's lines, which ride the control sidecar and are taken
/// off it here (`handoff_carry::ControlCarry::take_log`), before the rest of
/// the sidecar goes to the engine. No sidecar, or one without the member: no
/// lines, and the log starts over after its handoff gap.
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

/// THE ADOPTED LOG MEETS ITS ENGINE: after [`hydrate_adopted_engine`] and
/// before the reader starts, the engine's next block id and C count, read
/// under its lock, then handed to the session log under the timeline's (one
/// lock at a time), whose carried block cursor must fit them
/// (`session_log::SessionLog::fit_engine`): a screen that did not cross
/// leaves an engine whose block ids start over.
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

/// Install an adopted session's carried archive ([`hydrate_adopted_engine`]),
/// and say so when it carried rows that were refused (`AltArchive::import`:
/// an archive switched off here, or indices no archive reaches), so a
/// fresh `offscreen` after the update is named in the log.
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

/// The temporal recorder's first keyframe of session `id`'s engine, taken
/// before its reader runs. Returns whether one was recorded.
///
/// An adopted engine may be mid-sequence (a split CSI, an unfinished OSC), and
/// `checkpoint()` requires Ground, so this uses `Terminal::checkpoint_continuing`,
/// from which replay continues the sequence as the live engine does. A hooked
/// DCS (abandoned by updates) leaves the recorder unseeded, logged.
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

/// Install the fallback title (`record_title`: [`Adopted::title`], the park's
/// layout title or the handoff record's copy) when the checkpoint carried none, via
/// `Terminal::restore_title` (capped, controls stripped, title epoch bumped).
/// Returns `true` when installed; an empty fallback installs nothing.
///
/// Only for a checkpoint whose `title` is `None`, or no checkpoint: a carried
/// title, even an empty one, is the exact answer.
///
/// Run after [`hydrate_adopted_engine`], before the reader starts, so the
/// temporal keyframe and `App::register_session`'s title both see it.
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
/// ([`Terminal::set_console_host_posture`]), keyed on the resize seam policy the
/// session resizes under ([`crate::app_render::pty_resize_policy`]), so a `cast
/// drift` replay under that policy rebuilds the same engine.
///
/// conhost forwards a program's kitty push but cannot turn our CSI-u reports
/// into the key records a console program reads, so keys such as Ctrl+L and
/// Alt+a would arrive as nothing. A program with ENABLE_VIRTUAL_TERMINAL_INPUT
/// loses only kitty's extras. OpenConsole is no different, so both hosts get
/// this posture: with the protocol on for an OpenConsole session (1.24.2610.01001
/// and 1.25.2609.30003, 2026-10-17), a VT-input reader got every report intact,
/// but a record reader got Ctrl+L, Alt+a, Ctrl+A, Alt+Shift+B, Ctrl+Backspace
/// and Shift+Tab as the report's characters typed one by one. Only the host
/// knows which kind of reader reads next.
///
/// Gated on the backend (`aterm_pty::backend_is_conpty`), not `cfg(windows)`:
/// stub/replay sessions and Unix PTYs get the native posture. A conhost that
/// encodes key records as CSI-u would make this unnecessary for that backend.
///
/// In the same hold, the one recovery the two Windows hosts need differently,
/// keyed on the host this session's spawn really got (after any fallback to
/// conhost): under OpenConsole a dead app's alternate screen is left by the
/// host, not locally (`crate::alt_screen_resync`).
pub(crate) fn apply_pty_keyboard_posture(term: &Mutex<Terminal>, master: i32) {
    let policy = crate::app_render::pty_resize_policy(master);
    let host = aterm_pty::conpty_host(master);
    crate::alt_screen_resync::apply_posture(&mut term_lock(term), policy, host);
}

/// The `rlimit` set a daily-driver (`user`) shell is spawned with:
/// the launcher's, unchanged — except the soft descriptor limit, when this
/// process raised its own to claim a launched handoff's grant (round six of
/// the update audit, item 39, review round two) or to park its sessions for an
/// update (round eight, #3). That raise is the update's need, not its shells':
/// a tab opened after an update through the launched lane, or after an
/// attempt from this process, gets back the soft limit the process was
/// launched with (launchd's 256), as a Dock-launched aterm's tab does. The
/// hard limit is never touched.
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

/// Stand up one tab's shell session and start its PTY reader; shared by session
/// 0 and every new tab. Each session gets its own PTY master (using the shared
/// spawn/sandbox caps), a fresh shell-integration nonce when `integrate` is on,
/// its own OSC 52 authorization and clipboard thread, its own policy engine, and
/// its own reader thread tagging every `Wake` with this session's `id`.
/// Returns the `Session` or a spawn error (the caller decides whether it is fatal).
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
    // The owning window's cell size in device pixels, installed at construction
    // so DEC 1016 mouse reports and OSC 1337 image sizing are right before any
    // resize. See [`new_live_terminal`] for why the boot session passes `None`.
    cell_px: Option<(u16, u16)>,
    factory: &SessionFactory,
    proxy: &dyn WakeTarget,
    // Per-spawn working directory: a new tab / split / window passes the focused
    // pane's OSC 7 cwd here without mutating the shared `factory`. `None` falls
    // back to `factory.cwd` (the `-d <dir>` flag, else the launch directory).
    cwd_override: Option<&str>,
    // Per-spawn controller observation hint: `Some(origin)` injects
    // `ATERM_OBSERVE_SESSION_ID=<origin sid>` into the newborn's shell env
    // (identity only, see [`provision_observe_env`]). `None` for every
    // non-connected spawn.
    observe: Option<&SessionId>,
    // A WORKER's task (`spawn agent=claude`, `task_launch`): written to its
    // task file once the newborn's sid is known, the file named to the shell
    // as `ATERM_TASK_FILE`. `None` for every other spawn; ignored on an
    // adopted shell (its env is what it was).
    task: Option<&crate::task_launch::TaskSeed>,
    // `Some(_)` re-adopts a live shell handed across a seamless-update re-exec
    // (its PTY master fd, pid and identity) instead of forking a fresh one.
    mut adopt: Option<Adopted>,
) -> std::io::Result<Session> {
    // A warm spare (`crate::spare`): nobody asked for this shell yet.
    let spare = factory.spare.as_ref();
    // The coding-agent primer rides every FRESH spawn, throttled and detached,
    // so it costs this spawn nothing; a spare's runs at its adoption. An
    // adopted shell asks for none: the build that first spawned it primed its
    // agents in that build's words, which an update supersedes, so this
    // process's launch asked before it adopted anything (`PrimeCue::Launch`).
    prime_agents_if_due(PrimeCue::Spawn {
        adopted: adopt.is_some(),
        spare: spare.is_some(),
    });
    // The name the machine has now is the one this shell's `$HOST` will hold:
    // a citation's OSC 7 host is judged against it however the network renames
    // the machine later (`tab_label::names_this_machine`). One syscall.
    if adopt.is_none() {
        crate::tab_label::note_this_machine_name();
    }
    // A recovered master's release obligation, held to the registration at
    // the end: any failure on the way drops it, which releases the keeper's
    // copy (`keeper_link::Claim`).
    let mut keeper_claim = adopt.as_mut().and_then(|adopted| adopted.keeper.take());
    // What the crash restore brought back for a recovered master (its rung and
    // losses), recorded on the session for `status restore=` once it exists.
    let restore_report = keeper_claim
        .as_mut()
        .and_then(crate::keeper_link::Claim::take_restore);
    // A shell the keeper kept has no parked twin: no outgoing process resumes
    // it, and no Commit will come for it (a launch that took the keeper's
    // offers is no update's candidate; a degraded one boots as a plain window).
    // Its reader starts now and its re-key is written now, whatever the
    // overlap handoff defers for the sessions it carries.
    let recovered = keeper_claim.is_some();
    let handoff_local_id = adopt.as_ref().map(|adopted| adopted.local_id);
    let journal_key = adopt.as_ref().and_then(|adopted| adopted.journal_key);
    let frozen_path = adopt.as_ref().is_some_and(|adopted| adopted.frozen_path);
    // The re-key channel: an adopted shell has it when its record said so; a
    // fresh one gets it below, where its integration is prepared.
    let mut rekey_channel = adopt.as_ref().is_some_and(|adopted| adopted.rekey);
    // The integration loader (`shell_body`): an adopted shell has one when its
    // record said so or its checkpoint carried a signed revision; a fresh
    // zsh/bash/fish gets one below.
    let carried_rev: Option<String> = adopt
        .as_ref()
        .and_then(|adopted| adopted.checkpoint.as_ref())
        .and_then(|cp| cp.shell_integration_rev.clone());
    let mut body_loader =
        adopt.as_ref().is_some_and(|adopted| adopted.loader) || carried_rev.is_some();
    // What the handle carries for the fabric, the recorders, the supervisor
    // claims and the title, taken before the match below consumes it.
    let adopt_topics: Vec<String> = adopt
        .as_ref()
        .map(|adopted| adopted.topics.clone())
        .unwrap_or_default();
    let adopt_holds: Vec<String> = adopt
        .as_ref()
        .map(|adopted| adopted.holds.clone())
        .unwrap_or_default();
    // …and the carried mailbox and the timeline's last id (round seven).
    let adopt_fabric: crate::fabric::FabricCarry = adopt
        .as_ref()
        .map(|adopted| adopted.fabric.clone())
        .unwrap_or_default();
    let adopt_timeline_id: u64 = adopt.as_ref().map_or(0, |adopted| adopted.timeline_id);
    // …and the session log's epoch, node-id counters and seq count.
    let adopt_log: crate::session_log::LogAdopt = adopt
        .as_ref()
        .map(|adopted| adopted.log.clone())
        .unwrap_or_default();
    // …and the build that handed it across, for the handoff gap.
    let adopt_from_build: Option<u64> = adopt.as_ref().and_then(|adopted| adopted.outgoing_build);
    let adopt_title: String = adopt
        .as_ref()
        .map(|adopted| adopted.title.clone())
        .unwrap_or_default();
    let (adopt_supervisor, adopt_attention): (Option<String>, Vec<String>) = adopt
        .as_ref()
        .map(|adopted| (adopted.supervisor.clone(), adopted.attention_owners.clone()))
        .unwrap_or_default();
    // …and the task record (a worker's), re-seeded onto `ctx.timeline`.
    let adopt_task: Option<String> = adopt.as_ref().and_then(|adopted| adopted.task.clone());
    // …and the Autopilot's end, its face's time re-seeded onto `ctx.timeline`.
    let adopt_end: Option<String> = adopt
        .as_ref()
        .and_then(|adopted| adopted.autopilot_end.clone());
    // A fresh interactive shell that cannot start — a `$SHELL` or `shell` naming
    // nothing this user can run — is replaced by the account's login shell, else
    // `/bin/sh`, and the pane says so ([`substitute_shell`]): a launch from the
    // desktop menu otherwise ended with no window and the reason only in the
    // journal. Decided before the integration, which is prepared for the shell
    // that will run — and run without the `shell_args` the broken one was given.
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
    // Per-tab shell integration: a FRESH nonce per session. Reusing a nonce
    // across tabs would let tab A's (untrusted) output emit tab B's authorized
    // OSC 133/633 marks; a distinct nonce per engine prevents that cross-tab
    // forgery. Computed only when integration is enabled (never under `-e`).
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
                // WSL only: a POSIX cwd (what a WSL tab reports over OSC 7)
                // cannot be a `CreateProcessW` working directory, so the spawn
                // seam drops it and the new tab would land in aterm's own
                // directory. Hand it to the WSL-side launcher instead, which
                // `cd`s into it before exec'ing the login shell.
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

    // A substitute for a broken `$SHELL` is the shell the child's programs are
    // told to start too (`vim`'s `:shell`, `make`).
    if let Some(pair) = substitute.as_ref().and_then(|sub| sub.shell_env.clone()) {
        env_add.push(pair);
    }

    // Recursion provisioning: the root session adopts its fabric identity from
    // the injected env (so an outer aterm's preminted edges name us); other tabs
    // get a fresh one. Then mint a child identity and read/write/signal edges
    // for whatever this session spawns. This env is appended after the
    // shell-integration vars so it wins, and the deny-list strips inherited
    // copies so provisioning never replays past one hop.
    // `premint`: the root session answers to the identity the environment
    // injected, which a later launch from the same shell can adopt again (its
    // log's epoch is recorded beside the claim, below).
    let (self_id, self_nonce, premint) = match &adopt {
        // Adopted: restore the fabric identity from the handoff manifest so edges
        // and discovery stay valid. The id is kept unconditionally; the claim is
        // taken only once the predecessor has let go (`identity_claim::hold_transferred`).
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
    // The re-key channel (`shell_rekey`): a fresh integrated zsh/bash/fish is
    // told where a re-key would land (`<control dir>/rekey/<sid>`) and checks
    // that file at every prompt. The sid survives handoffs, so the path stays
    // valid. No private control dir: no channel.
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
    // A spare holds only where its shell can: an integrated zsh/bash told its
    // re-key path, from which it derives the FIFO's. The FIFO exists before the
    // fork (dropped, on any failure below, it is unlinked).
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
    // The body pointer (`shell_body`): a fresh integrated zsh/bash/fish is told
    // where a newer body would be named (`<control dir>/integration/<sid>`) and
    // its loader checks it at every prompt. No private control dir: no pointer.
    // The lost-nonce dev seam withholds it too.
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
    // A shell adopted with neither a loader in its record nor a signed revision
    // runs an integration from before loaders: nothing reaches it by itself, and
    // `status integration_rev=` says `frozen` (the typed upgrade can still reach
    // it — `shell_rekey::typed`).
    let integration_frozen = adopt.is_some() && !body_loader;
    // Identity is unconditional (except on adoption, where env cannot be
    // re-injected), so `@self` resolves from any descendant. Not gated on
    // `exec_command`: `aterm -e claude` needs it for the agent CLI's hooks.
    if adopt.is_none() {
        env_add.extend(provision_child_identity_env(&self_id));
        // Controller spawn: hand the supervisor the sid it observes (identity only).
        if let Some(origin) = observe {
            env_add.extend(provision_observe_env(origin));
        }
        // A WORKER: its task file, written now that the sid naming it is
        // known. A file that cannot be written fails the spawn: a worker
        // without its task is no worker.
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
    // A one-shot `-e <cmd>` session never hosts an inner aterm, so skip child
    // recursion provisioning entirely — the injected identity and the edge file
    // would be permanently unused. The session keeps the returned `ChildProxy`,
    // which removes the edge file when it drops.
    let child_proxy = if adopt.is_some() {
        // Adopted: the child keeps its first-spawn recursion tokens; env cannot be
        // re-injected, so nothing is re-provisioned.
        None
    } else if factory.exec_command.is_none() {
        // Edge-token secrets go to a 0600 file (path-only in env), which the
        // session keeps until it closes.
        let (recursion_env, child) = provision_child_proxy(control_auth::socket_dir().as_deref());
        env_add.extend(recursion_env);
        Some(child)
    } else {
        None
    };

    let adopted = adopt.is_some();
    // A fresh interactive shell records its start, so an exit moments later can
    // be told apart from one after it ran. Timed just before the fork: the
    // window starts no later than the shell does.
    let shell_started_at =
        records_shell_start(adopted, factory.exec_command.as_deref()).then(Instant::now);
    // The foreground holder the outgoing reader last saw, from the handoff
    // record; `0` (fresh shell or nothing carried) means probe afresh.
    let carried_fg_holder = adopt.as_ref().map_or(0, |a| a.fg_holder);
    // The render debt the outgoing engine owed the screen, seeded below.
    let carried_render_debt = adopt.as_ref().and_then(|a| a.render_debt);
    // A fresh shell's pid version at its spawn, for the spare gate
    // (`Witness::note_shell`); none for an adopted one.
    let mut shell_version = None;
    // THE HISTORY CARRY (`crate::handoff_history`): the sidecar opened before
    // the proof rides the session to Commit, when it is imported; its keys are
    // reserved as the engine is hydrated below. `None` for every fresh shell.
    let (master, sink, pid, adopt_checkpoint, mut adopt_control, adopt_repaint, mut adopt_history) =
        match adopt {
            Some(a) => {
                // `into_sink` re-arms CLOEXEC, as forkpty does for fresh masters.
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
                // The daily-driver mode (User, the default) inherits the
                // launching shell's limits so programs reserving large address
                // spaces are not constrained (`aterm_sandbox::Limits::inherit`);
                // Safety gets the soft caps, a guardrail its rc can raise, and
                // Containment the same caps as hard limits, which its program
                // cannot (`aterm_sandbox::Limits::contained`, ruling 2026-10-10).
                let limits = {
                    use aterm_containment::ContainmentMode as Cm;
                    match aterm_containment::mode_or_containment() {
                        Cm::User => daily_driver_limits(),
                        Cm::Safety => aterm_sandbox::Limits::shell_default(),
                        // Fail-safe to contained for an unrecognized mode.
                        _ => aterm_sandbox::Limits::contained(),
                    }
                };
                // A folder the shell cannot start in becomes the home folder,
                // reported once the shell is up.
                let start =
                    crate::spawn_folder::start_folder(cwd_override.or(factory.cwd.as_deref()));
                // A spare whose folder would fall back is no spare: nothing is
                // said for a shell nobody asked for.
                if spare.is_some() && start.fell_back.is_some() {
                    return Err(std::io::Error::other(crate::spare::FolderFallsBack));
                }
                // The fork waits here for the event loop to park, within the bound
                // the spawning thread asked for (`watchdog::forks_wait_for_park`;
                // none on the main thread): by now, past this spawn's own work,
                // the loop has parked.
                crate::watchdog::park_before_fork();
                // Capture the child pid (`spawn_shell_with_pid`) so `Session::drop` can HANG
                // UP the session (SIGHUP) before closing the master — the non-blocking
                // teardown that keeps the UI thread off the tty lock (see `Session::drop`).
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
                    // The same cell box fills the child's `ws_xpixel`/`ws_ypixel`,
                    // so a tool reading TIOCGWINSZ before any resize gets real geometry.
                    cell_px,
                );
                // Recursion provisioning (the `PROXIES` entry and edge-token file)
                // happened before this fallible spawn; on failure `child_proxy`
                // drops on the `?` and retires both, so failed spawns leak nothing.
                // A fall back to the Windows console host (aterm.toml `conpty`) is
                // handed to the log once, whether or not this spawn succeeded; never
                // anything off Windows.
                if let Some(note) = aterm_pty::take_console_host_fallback() {
                    aterm_log::warn!("console host: {note}");
                }
                // The kit and the provision this spawn took are replaced off this
                // thread, ready for the next (`spawn_prep`).
                crate::spawn_prep::poke();
                let aterm_pty::SpawnedShell { master, pid } = spawned?;
                // The shell's pid version for the spare gate, read first, the
                // moment its exec is confirmed: an `exec` in its startup files
                // takes a newer one than the launcher of anything the rc left
                // running before it (`spare::launched_before`).
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
    // Its driven input is read before anything could write a key to it.
    let shell_start = shell_started_at.map(|at| ShellStart {
        at,
        name: interactive_shell_name(shell_override),
        input: sink.driven_input(),
    });
    // Per-session asciicast v2 recorder, sized from this session's initial grid.
    // The header width/height start at this grid; a drop-oldest eviction of a
    // resize moves them to that resize (`CastRecorder::push`).
    let cast = Arc::new(std::sync::Mutex::new(crate::cast::CastRecorder::new(
        cols, rows,
    )));
    // Per-session temporal recorder: the hydratable event-log spine.
    let temporal = Arc::new(std::sync::Mutex::new(
        crate::temporal::TemporalRecorder::new(),
    ));
    // Per-session live byte fan-out: the reader thread tees every burst.
    let byte_fanout = Arc::new(crate::cast::ByteFanout::new());
    // The live engine ([`new_live_terminal`]), built before the reader starts.
    let term = Arc::new(Mutex::new(new_live_terminal(
        rows,
        cols,
        factory.terminal_config.as_ref(),
        cell_px,
    )));
    // …and hand its lock-free input-mode mirror to the session context: the seam
    // reads it per key press instead of taking the terminal mutex.

    // An adopted session continues with the carried turn ledger (records print
    // `carried=1`), or an empty one naming the turn ids it no longer vouches
    // for. Other sessions start empty.
    let observed = crate::session_watch::Observed::new(if adopted {
        crate::handoff_carry::adopted_ledger(adopt_control.as_mut())
    } else {
        crate::turn_ledger::TurnLedger::default()
    });
    // A host's hold of the engine that moves its frame rings the session's
    // screen readers (`TermGuard`).
    observed.watchers.bind_frame(&term);
    // The substitute's line goes in before the reader starts, so it stands above
    // the shell's first prompt.
    if let Some(sub) = &substitute {
        say_substitute(&term, sub);
    }

    // Per-session fabric identity (day-one single local session: a fresh id+nonce).
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
        update_parked: Default::default(),
        watchers: observed.watchers,
        frame: Default::default(),
        fenced_writes: Default::default(),
        // A spare's is its link's, which its reader and the main thread share.
        witness: spare.map_or_else(Arc::default, |s| Arc::clone(&s.link.witness)),
    });
    // The shell this session runs, for the spare's gate (its start record is
    // judged against it); a spare also holds its FIFO from here.
    if !adopted && integrated_shell.is_some() {
        ctx.witness.note_shell(pid, shell_version);
    }
    if let Some(fifo) = spare_fifo {
        ctx.witness.hold_fifo(fifo);
    }
    // ROOT session only: record the edges the OUTER aterm preminted for us (from
    // our injected env), so it holds the read/write/signal authority it granted.
    if id == 0 {
        register_injected_parent_edges(&ctx);
    }
    // THE HANDOFF GAP (round five, item 17): an adopted session's recorders
    // start here, empty, and say so — first, before the fabric and claim
    // seeds below write their rows.
    // The carried session log rides the control sidecar: taken off it here,
    // before the engine is hydrated with the rest (none for a fresh spawn).
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
    // THE EPOCH RECORD (`identity_claim`'s module doc): the log's epoch is now
    // this process's for good. An environment premint starts the record; an
    // id handed to this process keeps a record it already has above every
    // epoch it took.
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
    // Restore the carried fabric (broadcast opt-ins and standing halt) before
    // the session can be registered. Nothing for a fresh spawn.
    seed_adopted_fabric(
        &ctx.fabric,
        &ctx.timeline,
        &adopt_topics,
        &adopt_holds,
        &adopt_fabric,
    );
    // Restore the carried supervisor lease (so its next renewal lands here) and
    // keyed attention lines, on the fresh meta before registration. Nothing for
    // a fresh spawn.
    crate::session_timeline::seed_carried_claims(
        &ctx.meta,
        &ctx.timeline,
        adopt_supervisor.as_deref(),
        &adopt_attention,
        crate::metrics::now_us(),
    );
    // …and the task record a worker carried, after the claims (its attention
    // flag is the seeded meta's): a manager parked in `await task` re-arms on
    // the successor past each worker's rev from the start of its wait.
    // Nothing for a fresh spawn.
    crate::task_record::seed_carried(&ctx, adopt_task.as_deref());
    // …and the Autopilot's end (§11.2.3, item 8): its face's time held for
    // the loop's restated face. Nothing for a fresh spawn.
    seed_adopted_end(&ctx.timeline, adopt_end.as_deref());

    // Hydrate the adopted engine before the reader starts (no engine race) and
    // before the temporal keyframe (replay must rebuild the restored screen). The
    // engine may restore at the old grid size; the window's first resize
    // converges engine and PTY. The control carry's archive keeps its old origin
    // and indices, so the app's next frame diffs as the old process would have.
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
        // Before the reader starts or anything typed into the revealed window
        // is replayed: input past this point means the person took the session
        // back, and the viewport restore stands aside for them.
        adopt_history.watch_input(&ctx.output_echo);
        // The adopted log's block cursor against the engine it now reads.
        if adopted {
            fit_adopted_log(&term, &ctx.timeline);
        }
    }
    // THE HOLDER AFTER A RESTORE (card engine §3.8): a restored screen's
    // fence is re-derived from the kernel before a byte is fed — the carried
    // current mark continues when it is the holder's, an Exact drawer is
    // seeded from the running command's signed output start, anything else
    // is bare — and the image is published, so a Claude Code idle across the
    // restore is gated on the first frame with no new output.
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
    }
    // The render debt the outgoing engine owed, judged from attach as the parent
    // would have judged it (`seed_adopted_render_debt`).
    if let Some(debt) = carried_render_debt {
        seed_adopted_render_debt(&term, &ctx.timeline, debt);
    }
    // A checkpoint that carried no title takes the park's: without it the tab
    // reads as its directory until the program titles it again.
    let _ = seed_adopted_title(&term, adopt_checkpoint.as_ref(), &adopt_title);
    // ConPTY keyboard posture: after the hydrate (a restored checkpoint replaces
    // the engine's modes wholesale) and before the reader parses the first byte.
    apply_pty_keyboard_posture(&term, master);
    // Point an adopted shell's loader at this build's body unless it already
    // signs it; held for Commit under an overlap handoff, like a re-key.
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

    // Temporal seed: the initial keyframe, before any PTY output, so replay
    // hydrates from it and folds `RawIn` events forward. The spine's resize
    // watermark is seeded from the same hold as the checkpoint, so the reader
    // records every resize after the keyframe and none it already holds.
    if factory.temporal_recording {
        seed_temporal_keyframe(&term, &temporal, id);
    }

    // Trust only this tab's command marks: install its fresh nonce and require it.
    if let Some(nonce) = shell_nonce {
        let mut t = term_lock(&term);
        if !lost_nonce {
            t.authorize_shell_integration(nonce);
        }
        t.set_require_shell_integration_nonce(true);
    }

    // What aterm, unsandboxed, does at this tab's program's request: decided
    // once, from the mode its shell runs under ([`host_reach`]). A spare's
    // program reaches nothing: its requests only taint it until its adoption
    // arms the same wiring ([`arm_host_reach`]).
    let reach = session_host_reach();
    match spare {
        Some(s) => crate::spare::wire_spare_taint(&term, &s.link),
        None => arm_host_reach(&term, reach, factory, proxy, id, window, &ctx.witness),
    }

    // This tab's own `standard`-profile policy engine, installed before its
    // reader produces any bytes (fail-closed, as for session 0).
    term_lock(&term).apply_policy_engine(aterm_policy::engine::PolicyEngine::new(
        aterm_policy::profiles::standard(),
    ));

    configure_palette_refusal(&term);

    // THE CRASH-RESTORE JOURNAL (`crate::crash_restore`, on wherever the keeper link
    // is and its keeper takes companions): the session's region, laid out, with its
    // FIRST keyframe of the engine exactly as this spawn left it (fresh, or hydrated
    // from an adoption) — after every setup above, before the reader starts and
    // before the registration below tells the keeper the region exists. A region
    // that cannot be made leaves the session without one, which `status` says.
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
                // Its turn ledger rides the region's control slot (increment 7): an
                // adopted ledger is written now, before the registration below hands
                // the keeper the region, and every record from here on wakes the
                // journal worker to carry it.
                journal.carry_ledger(&ctx.turns);
                // Its history ring goes to the window's journal worker. An adopted
                // session's export waits for its history import (an update's sidecar
                // after Commit, a crash restore's ring after the recovery's restore
                // drain), which puts older lines below the carried ones: exporting
                // first would leave them out of this window's ring.
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
    // What a keeper recovery restored for this session, for `status restore=`.
    if let Some(report) = restore_report {
        ctx.crash_journal.note_restored(report);
    }

    // Wake coalescing: at most one `Wake::Output` in flight per session
    // (`gated_output_wake`). The value is the arm timestamp (ns on the latency
    // epoch; 0 = clear), which the lost-wake watchdog judges (`judge_output_wake`).
    let output_wake_pending = Arc::new(AtomicU64::new(0));

    // Per-session output-burst stamp (leading-edge ns on `lat_epoch`; 0 = clear),
    // so the present path books output-to-present only for sessions visible in
    // the presenting window (`App::present_latency_ns`).
    let last_output_ns = Arc::new(AtomicU64::new(0));

    // Per-session latest-output clock. Unlike `last_output_ns` it is overwritten
    // on every reader burst and never presentation-acknowledged: update admission
    // uses its age, so a hidden tab's old sample cannot block forever.
    let latest_output_activity_ns = Arc::new(AtomicU64::new(0));

    // Build the session first, then attach its byte pipeline: `attach_reader`
    // takes a built `Session` (it is also the overlap handoff's resume and
    // deferred-adoption primitive).
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
        // Recorded now, while the pid is known to be the shell: a fresh spawn is
        // our unreaped child; an adopted one must lead the carried PTY. Teardown
        // re-verifies it before any signal by pid.
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
        // `attach_reader` installs the real wake pipe alongside the reader.
        wake_wr: -1,
        reader_stop: Arc::new(AtomicBool::new(false)),
        reader_join: None,
        // `attach_reader` installs the writer join handle alongside the reader (only
        // when recording is enabled).
        temporal_writer_join: None,
        // Raised by `Session::drop` so an in-flight `aterm-reflow` worker
        // abandons its rewrap at the next bounded step instead of completing
        // it into a dead Terminal (see `drive_reflow_job`).
        reflow_cancel: Arc::new(AtomicBool::new(false)),
        // The foreground holder carried on the handoff record
        // (`Adopted::fg_holder`); `0` makes the reader probe.
        fg_holder: Arc::new(std::sync::atomic::AtomicI32::new(carried_fg_holder)),
        // The history carry, imported at Commit (`crate::handoff_history`);
        // `None` for a fresh shell.
        handoff_history: adopted.then_some(adopt_history),
        // A master of its own, fresh or adopted: counted in the PTY census. A
        // spare's is counted apart until its adoption swaps it.
        _pty_census: if spare.is_some() {
            crate::metrics::PtyCensus::spare()
        } else {
            crate::metrics::PtyCensus::counted()
        },
        // The incoming handoff's Commit (round seven, item 20): until it, the
        // parked predecessor owns an adopted shell, and dropping this session
        // must not signal it.
        handoff_commit: adopted_handoff_commit(adopted, recovered, factory.rekey_deferred.as_ref()),
    };
    // An adopted shell's exit status can only come from the kernel's witness;
    // registered now, while the recorded identity proves the pid leads the PTY.
    if adopted {
        session.exit_watch = adopted_exit_watch(&session.shell_identity);
    }

    // Overlap handoff: adopted sessions stay reader-less until every carried
    // window has presented and the readiness byte is written, so a child that
    // dies pre-ready consumed no PTY bytes and the parked parent resumes with
    // all of them (output waits in the kernel queue).
    // `App::maybe_signal_handoff_ready` attaches these readers. Fresh sessions
    // and shells the PTY keeper kept (`recovered`) attach here, except a tab
    // spawned off the event loop, whose reader starts parked until the tab is
    // installed (`SessionFactory::reader_after_install`).
    if !(adopted && factory.defer_adopted_readers && !recovered) && !factory.reader_after_install {
        match spare {
            // A spare's reader runs live (its rc's queries are answered), and
            // posts through the spare's link, which swallows every wake until
            // the adoption.
            Some(s) => attach_reader_with_post(&mut session, window, s.link.post(), factory),
            None => attach_reader_with_post(&mut session, window, proxy.wake_post(), factory),
        }
        .map_err(std::io::Error::other)?;
    }

    // An adopted alt-screen TUI restores as stale pixels it does not know to
    // repaint, and a repaint adoption's pane is blank: pulse the PTY size so the
    // kernel delivers SIGWINCHes (held only for a repaint adoption, so a
    // size-comparing program sees it). An exact main-screen restore skips it.
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
    // A repaint adoption's program may not answer the pulse (Claude Code reads
    // the size back unchanged): the repaint nudge's watch tells that apart from
    // one that did, once the reader is attached (`crate::app_nudge`).
    #[cfg(unix)]
    if adopt_repaint {
        crate::app_nudge::note_repaint_adoption(id);
    }
    // No seamless handoff off Unix, so nothing is ever adopted for a repaint.
    #[cfg(not(unix))]
    let _ = adopt_repaint;

    // THE PTY KEEPER (P3, opt-in `[keeper] enabled`): every master, fresh or
    // adopted, is registered with a close-on-exec duplicate — held until the
    // Commit in an update's candidate (`keeper_link::register`). One relaxed
    // load when the keeper is off.
    // A spare registers at its adoption: a crash before it must not let the
    // keeper restore a shell nobody asked for as a tab.
    if spare.is_none() {
        crate::keeper_link::register(&session, keeper_claim);
    }

    Ok(session)
}

/// Whether an adoption pulse holds its transient size ([`pulse_adopted_pty`]).
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PulseHold {
    /// Hold `rows-1` for [`ADOPTION_PULSE_HOLD`]: a repaint adoption, whose
    /// blank pane only a repaint fills.
    Held,
    /// The two halves back to back: an exact alt-screen restore, whose screen
    /// is already right. A program that redraws on any SIGWINCH (ncurses, vim)
    /// still does; a size-comparing one (Node) does nothing, which on an exact
    /// screen is correct.
    BackToBack,
}

/// The exit witness of a shell this process did not spawn. An adopted shell's
/// parent is the old build and then launchd, so `waitpid` cannot collect its
/// status. The kernel reports a same-uid non-child's `wait(2)` status through
/// `kqueue` ([`aterm_uds::exitwatch::ExitWatch`]), but only when registered
/// while it is alive, so it is registered at adoption, and only when
/// `identity` recorded a birth (`record_adopted_shell`). A watch on a pid that
/// no longer names the recorded shell is dropped unless the kernel already
/// recorded its exit. `None` on platforms without the primitive.
pub(crate) fn adopted_exit_watch(
    identity: &aterm_pty::ShellIdentity,
) -> Option<aterm_uds::exitwatch::ExitWatch> {
    identity.birth?;
    let watch = aterm_uds::exitwatch::ExitWatch::watch(u32::try_from(identity.pid).ok()?)?;
    (identity.verify() || watch.exit_status().is_some()).then_some(watch)
}

/// A `wait(2)`-encoded status as the exit it records: `exit(n)` or the signal
/// that ended the process. `None` for anything else (a stop, which is no exit).
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

/// The adoption size pulse: the PTY goes to `rows-1` and back to `rows` at the
/// engine's own grid, as SIGWINCHes the program sees; [`ADOPTION_PULSE_HOLD`]
/// apart for a [`PulseHold::Held`] pulse.
///
/// The engine's grid, not the spawn's `(rows, cols)`: a restored split pane is
/// spawned with the window's grid while its engine restores at the pane's size,
/// and no later winsize writer would correct it.
///
/// The hold: a size-comparing program (Node's `tty.WriteStream`, so Claude Code)
/// reads `TIOCGWINSZ` on a later loop turn and repaints only if the size
/// changed, so a repaint adoption holds `rows-1` on the PTY only, on a
/// short-lived thread (the engine keeps its grid, so no row is demoted). An
/// exact alt-screen restore is not held: drawing at `rows-1` would show its
/// layout one row short.
///
/// `adopt_pulse_hold` stands for the hold, set and cleared under the engine
/// lock; `sync_cell_pixel_size` leaves this PTY alone while it stands and
/// `resize_panes` writes only on a grid change. The second half writes the
/// engine's grid as it is then, under the engine lock, so a real resize during
/// the hold is not undone, and through a close-on-exec duplicate of the master,
/// so a session closed during the hold cannot hand the write to a reused fd
/// (that duplicate keeps the master open up to the hold's length after a close).
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
                // It takes the engine lock the UI thread contends: the floor.
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

/// How long a [`PulseHold::Held`] adoption pulse holds the PTY at `rows-1`:
/// long enough for a program to read the transient size even from a busy loop
/// turn (about 50 ms). Only held over a pane that is blank by design.
#[cfg(unix)]
pub(crate) const ADOPTION_PULSE_HOLD: std::time::Duration = std::time::Duration::from_millis(250);

/// [`pulse_adopted_pty`] with the hold injected: `hold` receives the second
/// half and must run it (production: at once, or after [`ADOPTION_PULSE_HOLD`]
/// on its own thread; a test: when it chooses). A `hold` that drops it unrun
/// ends the pulse at once, never at `rows-1`. `holding` stands from the first
/// half to the second (the session's `adopt_pulse_hold`).
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
        // A one-row grid has no smaller size to pulse through: the PTY is put
        // at the engine's grid and left there.
        aterm_pty::resize_with_cell_px(master, rows, cols, cell_px);
        return;
    }
    let duplicate = match aterm_pty::dup_fd(master) {
        Ok(fd) => fd,
        Err(error) => {
            // No duplicate to finish on later (fd exhaustion): pulse back to
            // back rather than not at all. A program that redraws on any
            // SIGWINCH still repaints; a size-comparing one may not, and that is
            // said here, where the reason is known.
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
        // Raised under the engine lock, so a same-size writer that reads it
        // there either wrote before the transient (which then overrides it) or
        // sees it and leaves the PTY to the second half.
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

/// The second half of an adoption pulse: puts the PTY back at the engine's
/// grid as it is when it runs, and lowers the session's `adopt_pulse_hold`.
/// Dropped unrun, it runs then — the pulse never ends at `rows-1`.
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
        // Under the engine lock: a resize that lands during the hold resized
        // the engine under it first, so this writes that size, never a stale
        // one; and a cell box a skipped same-size writer set is written here.
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
/// (`Adopted::render_debt`) and let the resize ledger look at it once, at
/// attach: the ledger opens an epoch on a debt no resize of its own explains,
/// with the risk wait counted from now (`resize_epoch::EpochBook::judge`).
/// Two lock scopes, never nested: the engine's (seed and sample), then the
/// timeline's.
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

/// Whether an adopted session's PTY gets the rows-1 → rows size pulse that makes
/// its program redraw: when it restored onto the alternate screen (a TUI whose
/// last full redraw died with the old window), or when its screen is blank on
/// purpose (`Adopted::repaint`). A main-screen shell restored exactly is left
/// alone: its line editor redrawing over an exact screen is only churn.
#[cfg(any(unix, test))]
pub(crate) fn adoption_needs_size_pulse(
    checkpoint: Option<&aterm_core::terminal::TerminalCheckpoint>,
    repaint: bool,
) -> bool {
    repaint || checkpoint.is_some_and(|cp| cp.modes.alternate_screen)
}

#[cfg(test)]
mod adoption_wiring_tests {

    /// An adopted session's timeline, cast and temporal recorder start empty and
    /// say so: the timeline's first row is `handoff carried=0 from_build=<n>`
    /// (`-` when the producer wrote no `outgoing_build`), and the cast header and
    /// `temporal status` disclose it. A fresh recorder is unchanged byte for
    /// byte. `spawn_session` marks every adopted session before any other row.
    #[test]
    fn an_adopted_timeline_opens_with_a_handoff_gap() {
        use std::sync::Mutex as StdMutex;
        let fresh_cast = crate::cast::CastRecorder::new(80, 24).to_asciicast();
        let fresh_temporal = crate::temporal::TemporalRecorder::new().status_tail();
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
            // …and a row recorded after it (the fabric seed, the registration).
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

    /// THE KEEPER'S RECOVERY RESTARTS THE COUNT, AND HIDES NOTHING FROM AN
    /// ANCHOR ABOVE IT (round eight of the update audit, U13): a session that
    /// crossed one seamless update counts on 65,536 ids above its producer's
    /// last ([`crate::session_timeline::HANDOFF_ID_RESERVE`]), so a driver
    /// tailing `timeline since=<last id>` holds an anchor that high. The PTY
    /// keeper's recovery of the same shell after that window crashed carries
    /// no count (`keeper_link`'s `timeline_id: 0`), so its rows start at 1.
    /// FAILS WITHOUT THE FIX: `since(anchor)` answered nothing for some
    /// 65,540 rows. NEGATIVE CONTROL: an anchor the recovery handed out
    /// itself still filters.
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
        // The window that crashed: the producer recorded 3 rows and handed
        // the session over; its successor recorded the `handoff` row and one
        // more, the driver's anchor.
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

    /// An adopted engine can start mid-sequence (a split CSI): its first keyframe
    /// carries the partial state, so replay finishes the sequence as the live
    /// engine does. Negative control: a hooked DCS query, which no keyframe can
    /// carry (its reply lives in the handler), leaves the recorder unseeded.
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

    /// Every kitty-derived bit of `KeyboardMode`.
    const KITTY_BITS: KeyboardMode = KeyboardMode::DISAMBIGUATE_ESC_CODES
        .union(KeyboardMode::REPORT_EVENT_TYPES)
        .union(KeyboardMode::REPORT_ALTERNATE_KEYS)
        .union(KeyboardMode::REPORT_ALL_KEYS_AS_ESC)
        .union(KeyboardMode::REPORT_ASSOCIATED_TEXT);

    fn live_engine() -> Mutex<Terminal> {
        Mutex::new(new_live_terminal(24, 80, None, None))
    }

    /// What an application sees when it probes and then pushes, as the live
    /// probe does: the reply to `CSI ? u`, then — after `CSI > 1 u` — the kitty
    /// bits the encoder runs under and the bytes Ctrl+L and Alt+a become.
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

    /// A master no PTY backend registered (GUI stub sessions, a closed one) and
    /// every Unix PTY keep the kitty protocol: the query is answered and the push
    /// takes. Non-vacuity control for the ConPTY test.
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

    /// A native session has no console host, so a program's DEC 9001 (a file
    /// `cat` writes, a remote over ssh) changes nothing: Escape and Shift+Enter
    /// keep their own bytes for whatever reads the tab next, through the
    /// lock-free word the input seam encodes from.
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

    /// A ConPTY session does not advertise the protocol: `CSI ? u` gets no reply
    /// and a push leaves keys legacy, so Ctrl+L and Alt+a stay `^L` and `ESC a`.
    ///
    /// The session is registered the adopted way (as `aterm_pty`'s
    /// `backend_is_conpty_follows_the_registry` does), so no shell is spawned.
    /// The "client process" is an event of ours, and `close_master` signals the
    /// session's close event so the waiter exits at once.
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
        // conhost's own `?9001h` at ConPTY start is honored on this backend.
        term_lock(&term).process(b"\x1b[?9001h");
        assert!(
            term_lock(&term)
                .keyboard_mode()
                .contains(KeyboardMode::WIN32_INPUT)
        );
    }

    /// The posture a session whose spawn got OpenConsole takes is conhost's:
    /// OpenConsole forwards the push, but a record reader (pwsh, cmd.exe) got
    /// each CSI-u report as its characters typed one by one (1.24.2610.01001
    /// and 1.25.2609.30003, 2026-10-17), so the protocol stays off there too.
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

    /// The pulse rule, all four ways.
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

    /// A restored split pane is spawned with the window's grid (24x80) while its
    /// engine restores at the pane's (24x39): the pulse leaves the PTY at the
    /// engine's size, or the program redraws at the window's width and nothing
    /// writes the pane size back. Read back through `TIOCGWINSZ` on a real pty.
    #[cfg(unix)]
    #[test]
    fn the_adoption_pulse_ends_on_the_engines_grid_not_the_windows() {
        let (_master_fd, _slave_fd, master, slave) = open_pty_pair();
        let pane = {
            let mut terminal = aterm_core::terminal::Terminal::new(24, 39);
            terminal.process(b"\x1b[?1049hvim");
            terminal.checkpoint_carry(0).expect("Ground")
        };
        // The spawn's engine is built at the window's grid, then restored.
        let term = std::sync::Arc::new(std::sync::Mutex::new(aterm_core::terminal::Terminal::new(
            24, 80,
        )));
        term.lock().expect("engine").restore_checkpoint(&pane);
        aterm_pty::resize_with_cell_px(master, 24, 80, Some((8, 16)));
        let holding = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        // Back to back (an exact alt-screen restore): over before it returns.
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
        // Held (a repaint adoption).
        let pulsed = std::time::Instant::now();
        super::pulse_adopted_pty(
            master,
            &term,
            &holding,
            Some((8, 16)),
            super::PulseHold::Held,
        );
        // The production hold ends the pulse on its own thread; wait for it
        // (a hang detector, not a latency budget).
        let deadline = pulsed + std::time::Duration::from_secs(60);
        let ended = loop {
            let ws = slave_winsize(slave);
            if ws.0 == 24 || std::time::Instant::now() > deadline {
                break ws;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        // The flag is lowered under the engine lock right after the write the
        // loop saw; take that lock so the read is ordered after it.
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

    /// A held pulse: with the hold injected, a program's read placed between the
    /// halves sees `rows-1`, the engine does not move (no demoted row, no
    /// journal), and the second half ends on the engine's grid as it is then,
    /// so a real resize during the hold is not undone.
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
        // The size the program cached before the park.
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
        // The program's SIGWINCH handler runs now, between the halves.
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

        // A real resize during the hold is not undone by the second half.
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

        // A hold that drops the second half unrun ends the pulse at once.
        super::pulse_adopted_pty_with(master, &term, &holding, Some((8, 16)), drop);
        assert_eq!((slave_winsize(slave).0, slave_winsize(slave).1), (40, 120));
        assert!(!holding.load(std::sync::atomic::Ordering::Acquire));
    }

    /// A PTY pair, both ends close-on-exec from birth
    /// (`aterm_pty::open_pty_pair`): `(master, slave)` owned, then their
    /// numbers.
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

    /// The winsize as the program reads it, through `TIOCGWINSZ` on the slave.
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
    /// Threads parked on this gate right now ([`DeferredReaderGate::parked`]).
    #[cfg(test)]
    parked: std::sync::atomic::AtomicUsize,
}

/// One thread's stay on a closed gate, counted while it lasts.
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

    /// How many threads are parked on this gate right now: each found it closed
    /// and unstopped, and leaves only by a release or its stop. A test waits for
    /// this before judging what a waiter has not done yet.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn parked(&self) -> usize {
        self.inner.parked.load(Ordering::Acquire)
    }

    /// Point-of-no-failure release, after the parent's Commit or once a tab
    /// spawned off the loop is registered (`tab_spawn`). All OS threads, channels,
    /// buffers, and wake descriptors were provisioned before it.
    pub(crate) fn release(&self) {
        self.inner.open.store(true, Ordering::Release);
        self.inner.ready.notify_all();
    }

    #[must_use]
    pub(crate) fn is_released(&self) -> bool {
        self.inner.open.load(Ordering::Acquire)
    }

    /// Wake closed-gate threads after their session stop flags were raised.
    /// Unlike `release`, this does not authorize a PTY read: each waiter
    /// observes `stop` and exits without touching the master.
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

    /// A shell with no integration is warned about once per name, not once per
    /// tab: the second `/bin/sh` tab is not new, a different shell is, and so is
    /// a different write failure.
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

    /// Which of `cues`, each at its second on one clock, start a pass.
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

    /// A SEAMLESS UPDATE'S SUCCESSOR adopts every tab, and its launch's pass
    /// is what puts this build's primer under the agents in them, and under
    /// each one the harness relaunches there. NEGATIVE CONTROL: the same boot
    /// without the launch's cue, as the build before asked, starts no pass
    /// however many tabs it adopts. A cold launch's fresh session 0 finds the
    /// launch's pass ran; a spare asks at its adoption, never at its spawn; a
    /// fresh tab a minute on asks again, and an adopted shell never does.
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

    /// The passes a launch's steps start, each with the step that started it:
    /// a cue at its second on one clock, or `None` for the first frame on
    /// glass. `held` is the launch's hold; without it every pass starts at its
    /// cue, as every launch's did before the hold.
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

    /// NO PASS STARTS BEFORE THE LAUNCH'S FIRST FRAME: the launch's cue, and
    /// session 0's fresh spawn behind it, start nothing until the frame is on
    /// glass, which starts the launch's pass, once; a cue after it starts its
    /// pass at once. A spawn the throttle lets through while held never
    /// replaces the launch's pass, a launch that took none starts session 0's
    /// at the frame, and one that took nothing starts nothing. NEGATIVE
    /// CONTROL: the same cold launch without the hold starts the launch's pass
    /// at its cue, ahead of session 0's fork.
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

    /// A headless instance never primes, and neither does a unit test; a
    /// windowed, non-test process does (the negative control: the gate is
    /// not simply closed).
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

    /// One pass against a scratch home: the knob is honoured before any read, a
    /// missing home is named, and a detected agent gets primed.
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
        // Under the process env lock with `$XDG_CONFIG_HOME` unset: the pass reads
        // it, and a parallel test that scopes it would otherwise lend this pass a
        // second detected agent writing into that test's scratch tree.
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

        // A dev bundle primes nothing unasked: enabled, with a home and a detected
        // agent, the pass from a non-release bundle writes nothing and says why.
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

/// A shell tab is told which shell it runs, and a `-e` session is told nothing:
/// the pair `aterm doctor` in the tab reads. The value is the spawn's own
/// resolution — `--shell cmd` is cmd.exe by absolute path, no flag at all is the
/// platform default — and the name is deny-listed, so an inherited copy never
/// survives into a tab that was not handed one.
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

/// A tab's integration is the one for the program its spawn resolves. On a
/// Windows machine with neither PowerShell on PATH the default tab is
/// `%COMSPEC%`'s cmd.exe, and it gets cmd's integration, not pwsh's; an alias
/// is resolved before it is named, so Git Bash's `gitbash` is bash.
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

    /// The live resolver: what a default tab runs is what it is integrated as.
    #[test]
    fn the_default_tab_is_integrated_as_the_program_it_runs() {
        let (program, _) = aterm_pty::select_shell_with_origin(None);
        assert_eq!(
            super::detect_spawn_shell(None),
            ShellType::detect(&program.to_string_lossy())
        );
    }
}

/// vim's `term_bg_default` calls a background dark when `COLORFGBG`'s last
/// field is 0-6 or 8, so a dark theme says `15;0`, a light one `0;15`, and an
/// unknown appearance is aterm's default, dark.
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

    /// Identity carries the parent id and the multiplexer baseline, nothing that
    /// grants authority: it is emitted for every session, one-shot `-e` included.
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
        // The baseline is the raw `<$TMUX>|<$STY>` pair, empty halves and all:
        // a session born outside any multiplexer must still carry a stamp, or
        // `ctl` cannot tell "never entered one" from "nobody told me".
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

    /// The CONTROLLER-spawn observation hint is the origin sid and NOTHING
    /// else (identity only, no token — design §1.4#3), and it is deny-listed
    /// so an inherited copy never survives a hop (one-hop like the rest of the
    /// provisioning vars).
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

    /// The union a shell session injects covers the parent id and the adoption
    /// identity, with no key emitted twice.
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

/// Tier-1 conformance of the real recursion provisioning to
/// `aterm_models_effects::capability_secrecy_model` (`CapabilitySecrecy`): a
/// child's edge-token secrets are published through a 0600 file, never the
/// inherited environment.
///
/// [`provision_child_proxy`] is driven with a private socket dir and with none,
/// through its composition over a minted provision
/// ([`provision_child_proxy_from`]), so the secrets are the ones minted.
/// `published` is read off what it produced: `2` (env channel) if any of the
/// child's three secrets appears in an env value, `1` (file channel) if the env
/// names a file holding exactly them, else `0`. The observed `Provision` must be a committed-model
/// step, and the file channel must be used exactly when there is a private dir.
///
/// [`provision_child_proxy`] runs with a private socket dir and with none;
/// `published` is `2` (env) if any of the registered secrets appears in an env
/// value, `1` (file) if the env names a file holding exactly them, else `0`. The
/// observed `Provision` must be a committed-model step, on the file channel
/// exactly when there is a private dir. `SandboxedRead` is not validated: the
/// model derives the peer's holding from the channel. Negative control: the
/// hexes in the env land `Provision` on `published = 2`, which only `Buggy = 1`
/// takes.
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
            // Held to the iteration's end: its drop removes the file.
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
        // The negative below is meaningful only once the waiter is provably
        // parked on the closed gate: a FACT the gate counts, not a window.
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
            // A hang detector: a gate that never releases never sends.
            rx.recv_timeout(std::time::Duration::from_secs(60)),
            Ok(true),
            "release is idempotent and requires no resource acquisition"
        );
        join.join().unwrap();
    }
}

/// (Re)attach the live byte pipeline to `session`: the helper threads (cast /
/// temporal / reply / compress) and the PTY reader on a fresh wake pipe. The
/// helpers are spawned with the reader because it holds their only senders.
///
/// Callers:
/// * every normal `spawn_session`, right after the `Session` is built;
/// * `App::attach_deferred_readers` (via `resume_deferred_readers_nonblocking`):
///   an overlap-handoff child's deferred adopted readers, a park-deadline
///   straggler, and the parent's parked readers after a failed handoff
///   (`App::rollback_overlap`).
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

/// [`attach_reader`] with the reader's posts going to `post` (a warm spare's
/// link, `crate::spare::SpareLink::post`, or a spawn's [`WakeTarget`]): live, no
/// start gate.
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

/// Provision every adopted reader/helper resource behind one closed gate. Returning
/// `Ok` proves Commit can activate this session using only an atomic store + condvar
/// notify; no post-Commit spawn/allocation/file-descriptor failure remains.
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

/// Hand a fresh reader clean shared latches, so nothing it does is governed by
/// state the previous reader left.
///
/// `reader_stop`: [`park_reader`] raises it, and a reader starting with it set
/// would exit at once.
///
/// `output_wake_pending`: armed by the reader, cleared by the `Wake::Output`
/// handler. A park can leave it armed with a wake no handler will consume; the
/// new reader would then hit [`gated_output_wake`]'s armed fast path and
/// suppress every wake, freezing the screen until the lost-wake watchdog heals
/// it. Cleared after the old thread is joined and before the new one spawns,
/// so an arm is owned only by the reader that posted it.
///
/// Not `SessionCtx::update_parked`: a park ends when the new reader has caught
/// up with what queued while none ran ([`UpdatePark`]), not at its attach, and
/// that reader rings the session's watchers as it ends it. ConPTY's reader
/// never parks, so it ends nothing.
fn reset_reader_latches(session: &Session) {
    session.reader_stop.store(false, Ordering::Release);
    // Relaxed matches the latch protocol everywhere else: grid content is
    // synchronized by the term mutex, the latch only governs wake delivery.
    session.output_wake_pending.store(0, Ordering::Relaxed);
}

/// Where a PTY reader posts its `Wake`s; `false` when the event loop is gone.
/// Shipping attaches use the event-loop proxy ([`post_to`]); the Tier-1 reader
/// bind (`dsu_quiescence_conformance`) uses its own sink. An `Arc` because the
/// attach also hands a clone to the sink's input hook, which posts
/// `Wake::InputWatch` from whichever thread wrote (`input_stall::watch_input`).
pub(crate) type WakePost = Arc<dyn Fn(Wake) -> bool + Send + Sync>;

/// The shipping [`WakePost`]: the event loop's proxy.
pub(crate) fn post_to(proxy: &EventLoopProxy<Wake>) -> WakePost {
    let proxy = proxy.clone();
    Arc::new(move |wake| proxy.send_event(wake).is_ok())
}

/// Where [`spawn_session`] wires a session's reader and host callbacks to post: the
/// event loop's proxy, or, for session 0, which is spawned before the loop is built
/// (`main_entry`), a sink that holds every post until the loop exists (`LaneSink`).
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
    // The engine asks the host, lazily, whether the shell runs a program it
    // started since its prompt where no foreground group shows one
    // (`SessionFactory::program_probe`): an adopted engine gets it here too.
    {
        let (probe, shell) = (factory.program_probe, session.pid);
        crate::term_lock(&session.term)
            .set_program_probe(Some(Box::new(move |since_ms| probe(shell, since_ms))));
    }
    // The reader records each burst into the session's cast recorder itself. Its
    // epoch (a burst's time goes on the recorder's timeline) and its watermark
    // (each hold counts itself there) are taken once here, so neither a hold nor a
    // timestamp takes the recorder lock.
    let (cast_epoch, cast_watermark) = {
        let rec = session.ctx.cast.lock().unwrap_or_else(|p| p.into_inner());
        (rec.epoch(), rec.watermark())
    };
    // The temporal writer and reader taps exist only when recording is enabled.
    // A straggler writer from a prior attach (its reader was joined above) is
    // joined before a fresh one spawns into the same recorder, so appends stay
    // ordered.
    if let Some(old_writer) = session.temporal_writer_join.take() {
        let _ = old_writer.join();
    }
    let (temporal_tx, temporal_writer_join, spine_mark) = if factory.temporal_recording {
        // The spine records each resize's geometry only; the seam policy the
        // engine resizes with is the backend's, so the replay takes it from
        // here, and the host's alternate-screen recovery with it. Asked BEFORE
        // the recorder lock: each answer takes the PTY registry's own lock,
        // which must not nest under this one.
        let policy = crate::app_render::pty_resize_policy(session.master);
        let host_resyncs =
            crate::alt_screen_resync::host_resyncs(aterm_pty::conpty_host(session.master));
        // This reader's resize watermark starts where the session's previous
        // reader left the spine (both it and its writer were joined above), so
        // a resize that landed while no reader was attached is recorded from
        // the engine's journal, or counted as lost — never skipped silently.
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
    // Dedicated reply-writer thread: the reader hands query replies here instead
    // of writing them inline, so it never parks on the input-pipe write.
    // The join handle is dropped (detached): the thread ends on its own when its
    // reader drops the sender or the sink drops (see `spawn_reply_writer`).
    let (reply_tx, _reply_writer_join) = spawn_reply_writer(
        session.ctx.sink.clone(),
        ReportRoute::of(&session.term, &session.ctx.modes),
    )?;
    // Input watch: a write that lands in the kernel's input queue while the
    // watch is unarmed posts `Wake::InputWatch` once, and the main thread probes
    // whether the program reads it (`App::observe_input_stalls`); this attach
    // does too when the master arrives with unread input (an adopted session).
    // `watch_input` installs nothing where the probe cannot answer (off macOS,
    // off a tty). The hook runs under the sink's fd lock, so `send_event` must
    // be a non-blocking post.
    {
        let post = Arc::clone(&post);
        let id = session.id;
        let _ = crate::input_stall::watch_input(&session.ctx.sink, move || {
            let _ = post(Wake::InputWatch { session: id });
        });
    }

    // Dedicated tier-compression worker. The engine offload is activated only
    // when the worker spawns, so a spawn failure falls back to the inline drain
    // (the reader promoting at the 1000-line threshold). Set both ways: a
    // re-attach whose worker fails deactivates a previous attach's offload.
    let compress_tx = spawn_compress_worker(session.term.clone(), session.ctx.ui_waiting.clone());
    crate::term_lock(&session.term).set_compress_offload_active(compress_tx.is_some());

    // Wake resources are created only after every helper thread exists. A
    // missing pipe is still safe (bounded-poll stop fallback); a reader-thread spawn
    // failure closes both raw descriptors before returning.
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
    // ConPTY has no foreground process group: the Windows reader takes no probe,
    // keeps no holder, reads no holder image and runs no byte arm.
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

/// True when a parked/deferred session can be reattached without blocking the
/// caller on either its old PTY reader or temporal writer. The event-loop rollback
/// path polls this predicate via a delayed Wake, keeping its latency bounded.
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

/// Whether a session's parser is behind its program because of an update's park
/// (`SessionCtx::update_parked`): from [`park_reader`]'s stop until a reader
/// attached after it has handed the parser everything the program wrote while no
/// reader ran ([`GatherMsg::CaughtUp`]). While it stands, the control waits hold
/// their idle clocks and their submit windows (round seven of the update audit,
/// findings 62 and 63), and `ready` reads no prompt off the parser (round eight's
/// lead U12): the program's output since the park waits unread in the kernel, so
/// neither the parser's silence nor its last prompt is the program's.
///
/// Ended by the reader, not by the attach: a reattached reader has read nothing
/// yet, and a `make` typed at a prompt during the park has signed its start into
/// the queue. Each park takes a fresh mark, and a reader ends only the park it was
/// attached after, so a reader still catching up when the next park begins
/// cannot end that one.
#[derive(Debug, Default)]
pub(crate) struct UpdatePark(std::sync::atomic::AtomicU64);

/// The marks [`UpdatePark::park`] hands out: never 0, which is "not parked".
#[cfg(any(unix, test))]
static UPDATE_PARK_MARKS: AtomicU64 = AtomicU64::new(1);

impl UpdatePark {
    /// Whether the parser may be behind the program by output a park left unread.
    pub(crate) fn is_parked(&self) -> bool {
        self.0.load(Ordering::Acquire) != 0
    }

    /// A park begins: nothing consumes this session's output from here.
    #[cfg(any(unix, test))]
    fn park(&self) {
        self.0.store(
            UPDATE_PARK_MARKS.fetch_add(1, Ordering::Relaxed),
            Ordering::Release,
        );
    }

    /// The park a reader attached now catches up from (0: none).
    #[cfg(unix)]
    fn mark(&self) -> u64 {
        self.0.load(Ordering::Acquire)
    }

    /// The reader attached at `mark` has handed the parser everything that queued
    /// before it ran: that park ends, never a later one. `true` when this call
    /// ended it, which the reader rings the session's watchers for, so a parked
    /// wait takes up its clocks at the resume. ConPTY's reader never parks.
    #[cfg(unix)]
    fn caught_up(&self, mark: u64) -> bool {
        self.0
            .compare_exchange(mark, 0, Ordering::Release, Ordering::Relaxed)
            .is_ok()
    }

    /// A park as the control waits see one, for their tests.
    #[cfg(test)]
    pub(crate) fn park_for_test(&self) {
        self.park();
    }

    /// The reader caught up, for the control waits' tests.
    #[cfg(test)]
    pub(crate) fn resume_for_test(&self) {
        self.0.store(0, Ordering::Release);
    }
}

/// Park a session's PTY reader, the overlap handoff's quiescence step. Pokes
/// the wake pipe (the reader breaks quietly, as on `Session::drop`: no
/// `Wake::Exit`, no tab close, master untouched) and waits until `deadline`
/// for the thread to finish, proving this process no longer consumes PTY
/// bytes, so the next checkpoint is gap-free and later bytes wait in the
/// kernel queue. Returns `false` when the reader did not exit in time; the
/// caller may proceed with the degraded exec-grade race. The rollback's
/// deferred resume reattaches only after the straggler has finished.
#[cfg(any(unix, test))]
pub(crate) fn park_reader(session: &mut Session, deadline: Instant) -> bool {
    let Some(join) = session.reader_join.take() else {
        return true; // never attached, already parked, or deferred
    };
    // From the stop on, nothing here consumes this session's output: say so to
    // the control waits before the reader is told (`SessionCtx::update_parked`),
    // and ring them, so a parked wait holds its clocks from the park's onset (a
    // `turn`'s submit window is stretched by the time spent parked).
    session.ctx.update_parked.park();
    session.ctx.watchers.ring_watchers();
    if session.wake_wr >= 0 {
        aterm_pty::wake(session.wake_wr);
    }
    // Fd-free fallback (wake pipe missing under fd exhaustion): the reader's
    // bounded poll checks this flag. `attach_reader` re-arms it to `false`.
    session
        .reader_stop
        .store(true, std::sync::atomic::Ordering::Release);
    while !join.is_finished() {
        if Instant::now() >= deadline {
            // Keep the handle: `attach_reader` must join it before respawning.
            session.reader_join = Some(join);
            return false;
        }
        crate::watchdog::beat(crate::watchdog::Breadcrumb::UpdateHandoff);
        // Yield rather than sleep: a 1 ms sleep can oversleep well past
        // `deadline`. The clock is re-read every iteration.
        std::thread::yield_now();
    }
    // `is_finished` makes this join non-blocking. Consuming the finished handle
    // (instead of merely dropping it) is the reader-park ACK's synchronization
    // edge: every release-stamped output activity write happens-before the
    // post-park acquire recheck in `start_unix_update_handoff`.
    if join.join().is_err() {
        // A panicked reader did not prove an orderly final consume/stamp edge.
        // Fail closed so automatic handoff rolls back and reattaches a reader;
        // never treat thread termination alone as a successful quiescence ACK.
        return false;
    }
    // The reader has exited and dropped its temporal sender, so the writer's `recv`
    // loop ends after draining its backlog. Wait for it under the same deadline,
    // so a re-attach cannot spawn a second writer into the same recorder while
    // this one is still appending.
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

/// Tier-1 bind for `ForegroundHandback`: the real reader, a scripted foreground
/// probe, and the modelled schedules.
#[cfg(all(test, unix))]
#[path = "foreground_handback_conformance.rs"]
mod foreground_handback_conformance;

/// Which cwd report sets a tab's folder, through the real reader: its
/// per-slice foreground word and the program probe it installs.
#[cfg(all(test, unix))]
#[path = "cwd_report_reader_tests.rs"]
mod cwd_report_reader_tests;

/// The holder image through the real reader (card engine I1): published
/// before the hold of its first byte, by a group edge, an `exec` after a
/// signed C and a launcher's `exec`.
#[cfg(all(test, unix))]
#[path = "holder_image_reader_tests.rs"]
mod holder_image_reader_tests;

/// THE TAB ENDS WITH ITS SHELL (docs/AUDIT-pty-render-2026-10-09.md P3, in the
/// window's reader). The real reader on a real pseudo-terminal whose slave the
/// test holds, standing in for the job a shell left behind (`cmd &` and
/// `exit` under bash's default `huponexit`): the master never reaches EOF,
/// which on Linux is every such exit, since Linux does not hang a PTY up at
/// its session leader's exit. The session's shell is a real child; when it
/// exits, the reader passes on what the master held and posts `Wake::Exit`,
/// the tab's close. Before, only the master's EOF did, so the tab stayed open
/// on a dead shell until the job let go. The reader goes on reading after
/// that report, so a pane `--hold` keeps shows what the job writes later (and
/// the job never blocks on an unread terminal), and the job's letting go then
/// ends the reader without a second exit.
#[cfg(all(test, unix))]
mod shell_exit_tests {
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
        // The shell's last words, then its exit; the job keeps the slave.
        // SAFETY: a write of a live buffer to the test-owned slave.
        let wrote = unsafe { libc::write(slave, b"bye".as_ptr().cast(), 3) };
        assert_eq!(wrote, 3, "write the shell's last words");
        drop(shell.stdin.take());
        // A hang detector: the shell's exit is the event.
        let exited = next("exit");
        let _ = shell.wait();
        let row = |session: &crate::Session| {
            crate::term_lock(&session.term)
                .row_text(0)
                .unwrap_or_default()
        };
        let shown = row(&session);
        // The job writes on after the exit, as into a held pane.
        // SAFETY: a write of a live buffer to the test-owned slave.
        let wrote = unsafe { libc::write(slave, b"late".as_ptr().cast(), 4) };
        assert_eq!(wrote, 4, "write the job's later output");
        let mut late_shown = row(&session).starts_with("byelate");
        let until = std::time::Instant::now() + patience;
        while exited && !late_shown && std::time::Instant::now() < until {
            std::thread::yield_now();
            late_shown = row(&session).starts_with("byelate");
        }
        // The job lets go: the reader ends on the master's EOF.
        aterm_pty::close_fd(slave);
        let reader_ended = session.reader_join.as_ref().is_some_and(|reader| {
            let until = std::time::Instant::now() + patience;
            while !reader.is_finished() && std::time::Instant::now() < until {
                std::thread::yield_now();
            }
            reader.is_finished()
        });
        // Stop a reader that never ended, then free the master.
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

    /// The gather's wait for its shell's retirement (before it posts the exit)
    /// reaps nothing: a shell still running at the bound is left running, and
    /// one that exited is left collectable — its status is the session's one
    /// reap to take, which the UI thread then takes without waiting.
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
            watch: aterm_uds::exitwatch::exit_fd(pid).expect("the shell is watched"),
            pid,
        };
        exit.await_retired(std::time::Duration::from_millis(1));
        assert!(
            shell.try_wait().expect("a look at the shell").is_none(),
            "a shell still running is left running"
        );
        drop(shell.stdin.take());
        // A hang detector: the shell's exit is the event.
        exit.await_retired(aterm_spec::wait::MUST_HAPPEN);
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
}

#[cfg(test)]
mod park_reader_tests {
    use super::park_reader;

    /// Thread termination is not sufficient proof of an orderly reader park. A
    /// panic must fail the handoff ACK so the caller rolls back and reattaches.
    #[test]
    fn panicked_reader_fails_closed_instead_of_authorizing_handoff() {
        let mut session = crate::stub_session(77);
        session.reader_join = Some(std::thread::spawn(|| {
            panic!("injected PTY reader failure");
        }));
        let waits = crate::session_watch::Subscription::single(&session.ctx.watchers);

        // A generous deadline: a timeout would also return false, without
        // exercising the panic path.
        assert!(!park_reader(
            &mut session,
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        ));
        assert!(session.reader_join.is_none());
        // Parked, whatever the verdict: the control waits hold their clocks
        // (round seven, findings 62 and 63), from the onset the park rang.
        assert!(session.ctx.update_parked.is_parked());
        assert!(
            waits.wait(std::time::Duration::ZERO),
            "the park rings the session's waits"
        );
        // NEGATIVE CONTROL: a reader already parked parks nothing, and rings
        // nobody.
        assert!(park_reader(
            &mut session,
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        ));
        assert!(!waits.wait(std::time::Duration::ZERO));
    }

    /// A park leaves the wake latch armed with an event no handler will consume.
    /// Re-attach must hand the new reader a clear latch, or `gated_output_wake`'s
    /// armed fast path swallows every wake and the screen freezes.
    #[test]
    fn a_reattached_reader_never_inherits_a_dead_wake_arm() {
        let session = crate::stub_session(78);
        // The stale arm the old reader left (an unsent arm, which the watchdog
        // never presumes lost on its own).
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
        // The observable consequence, not just the field value: the very next
        // burst's wake actually reaches the event loop.
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

    /// Past the deadline nothing is proven stopped: `park_reader` answers
    /// `false` and keeps the unfinished handle, the reader's or else the
    /// temporal writer's, for `attach_reader` to join before it respawns
    /// (`reader_attach_ready`), so a second reader or writer never runs beside
    /// it. The session is parked all the same. NEGATIVE CONTROL: once the kept
    /// reader has finished, the next park takes it and answers `true`; once the
    /// kept writer has, the attach's readiness poll says so.
    #[test]
    fn a_park_past_its_deadline_keeps_the_unfinished_handle() {
        let mut session = crate::stub_session(79);
        let (finish_reader, reader_held) = std::sync::mpsc::channel::<()>();
        session.reader_join = Some(std::thread::spawn(move || {
            let _ = reader_held.recv();
        }));
        // A deadline already passed, and a reader blocked until it is told.
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

        // The reader has stopped, its writer has not.
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
        // No reader to park: only the writer's handle, observed again by the
        // attach's own readiness poll.
        let joined = aterm_spec::wait::within(
            aterm_spec::wait::MUST_HAPPEN,
            std::time::Duration::ZERO,
            || super::reader_attach_ready(&session).then_some(()),
        );
        assert!(joined.is_some(), "timed out waiting for the writer to end");
    }
}

/// ROUND EIGHT OF THE UPDATE AUDIT, LEAD U12: an update's park ends when the
/// reader attached after it has handed the parser what the program wrote
/// while none ran, not at the attach ([`super::UpdatePark`]). Until then the
/// parser speaks for the program as it was before the park: a shell at its
/// prompt that was handed `make` meanwhile has signed the command's start into
/// the kernel queue, and a `ready` that read the prompt then answered "ready"
/// with `make` running. The kernel's queue depth (`TIOCOUTQ`) is macOS's.
#[cfg(all(test, target_os = "macos"))]
mod update_park_catch_up_tests {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::time::Instant;

    use aterm_core::terminal::BlockState;

    use super::{DeferredReaderGate, UpdatePark, attach_reader_inner, park_reader};
    use crate::{App, Session, Wake, WindowId};

    /// Every wait here is a hang detector, never a latency budget.
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

    /// The program at the slave writes `bytes`.
    fn program_writes(slave: i32, bytes: &[u8]) {
        // SAFETY: a write of a live buffer to the test-owned slave.
        let wrote = unsafe { libc::write(slave, bytes.as_ptr().cast(), bytes.len()) };
        assert_eq!(
            usize::try_from(wrote).ok(),
            Some(bytes.len()),
            "the program wrote"
        );
    }

    /// A real session on a real pseudo-terminal: its reader attached (wakes to
    /// nowhere), at a shell-integration prompt, then parked for an update.
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

    /// FAILS WITHOUT THE FIX: the attach ended the park, so the waits read a
    /// prompt the program had left, while its `make` waited unread — and,
    /// below, while the reader held that `make` and had not parsed it.
    #[test]
    fn a_resumed_reader_ends_the_park_only_once_its_parser_holds_what_queued() {
        let app = App::headless_for_test();
        let (mut session, slave) = parked_at_a_prompt(&app, 93);
        // Parked, the shell is handed `make`: its start and output queue unread.
        program_writes(slave, b"\x1b]133;B\x07make\r\n\x1b]133;C\x07building...");
        assert!(aterm_pty::output_queue_len(session.master).is_some_and(|n| n > 0));

        // The rollback reattaches a reader: behind a gate here, so the test holds
        // the terminal before anything is read, and without the byte arm, whose
        // seed takes the terminal as the parse stage starts.
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

        // The parse stage takes up the batch that carries what queued, and
        // waits there on the terminal the test holds. Its first-edge stamp is
        // taken before that wait (re-opened here, as a present does): by then
        // every message before the batch is handled.
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

    /// NEGATIVE CONTROL: with nothing queued, the reattached reader owes the
    /// parser nothing and ends the park as it starts, with no output to wait
    /// for.
    ///
    /// The end of the park is an event: the parse stage rings the session's
    /// watchers as its catch-up ends the park, so a parked wait takes up its
    /// clocks at the resume rather than at its next poll. Nothing is queued,
    /// so no burst rings here: FAILS with the catch-up's ring removed. Its own
    /// negative control: a reader attached after a park that a later one has
    /// superseded holds a stale mark, so its catch-up ends nothing and rings
    /// nobody (FAILS with a ring that does not ask `caught_up`).
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

        // The next park, which rings as it begins.
        assert!(park_reader(&mut session, Instant::now() + PATIENCE));
        assert!(
            waits.wait(std::time::Duration::ZERO),
            "the park rings the session's waits"
        );
        // A reader attached after it, held at its gate while a later park
        // supersedes its mark.
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
        // Ready is the parse stage's answer to `Started`: the gather has passed
        // its gate and sends `CaughtUp` next, whatever follows.
        assert!(
            ready.recv_timeout(PATIENCE).is_ok(),
            "timed out waiting for the reader to start"
        );
        // Stopped without a park (which would ring): the parse stage ends only
        // after every message before the stop, the catch-up included.
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

    /// A reader still catching up when the next park begins ends only the park
    /// it was attached after. FAILS with a catch-up that clears the flag
    /// outright: the second park reads as over while nothing consumes output.
    /// Only the catch-up that ends a park reports it, so the reader rings the
    /// session's watchers once per resume and never for a stale mark.
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
        // A reader attached to a session never parked ends nothing.
        park.park();
        assert!(!park.caught_up(0));
        assert!(park.is_parked());
    }
}

/// Build the engine for a fresh live session: the configured grid (scrollback,
/// cursor, theme, palette) plus DEC mode 1007 (alternate scroll) on by default,
/// so pagers that never request mouse tracking (less, man, git log) scroll
/// under the wheel. Programs can toggle it (`CSI ?1007l`/`h`); DECRQM reports it.
///
/// `cell_px`: the window's real cell size in device pixels, read by DEC 1016
/// mouse reports and OSC 1337 image sizing. `None` keeps the engine's 8x16
/// placeholder and is reserved for the boot session, whose backend is still
/// being built (`Backend::cell_geometry` on a `Pending` slot panics);
/// `App::sync_cell_pixel_size` corrects it at the first `apply_window_scale`.
pub(crate) fn new_live_terminal(
    rows: u16,
    cols: u16,
    cfg: Option<&aterm_core::config::TerminalConfig>,
    cell_px: Option<(u16, u16)>,
) -> Terminal {
    // A tiered in-memory scrollback store (hot / warm-LZ4 / cold-LZ4, bounded by
    // the memory budget): without one, `apply_config`'s limit/budget setters
    // no-op and history is capped at the grid ring. The limit is one total across
    // store and ring (`scrollback_lines = 0`: unlimited, bounded by the budget).
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
    // CELL-PX-1: the real font metrics of the window this session is born into, so
    // the very FIRST DEC 1016 report and the very FIRST OSC 1337 image on a
    // never-resized tab are already correct (see this function's docs). Clamped to
    // 1 because a zero axis is a divide-by-zero for the image footprint math.
    if let Some((cw, ch)) = cell_px {
        t.set_cell_pixel_size(cw.max(1), ch.max(1));
    }
    // Set as the host baseline too: the foreground handback restores 1007 to it.
    t.set_host_alternate_scroll(true);
    // The alt-screen archive's origin: indices mean something only inside the
    // archive that minted them, so a mark from a previous process is never read
    // as an index here. An adopted session with a carried archive takes the
    // carried origin back (`AltArchive::import`).
    t.set_alt_archive_origin(alt_archive_origin());
    // The archive's 4 MiB budget is shared across the app: each live session
    // gets `4 MiB / live` (floor 256 KiB), and dropping the terminal returns its
    // share. Each session evicts only its own rows, so no cross-session lock.
    t.set_alt_archive_shared(true);
    t
}

/// This process's alt-screen archive origin, stamped on every live session's
/// archive ([`new_live_terminal`]) and printed by `offscreen origin=` and the
/// turn ledger's `arch=<origin>:<last>` marks: `pid << 32 ^ launch nanos`,
/// fixed at the first call and never 0.
///
/// Archive indices restart at 1 in every archive, so `offscreen
/// since=<origin>:<i>` names the origin an index was minted under; a mismatch
/// reads from the start and `origin=` says why. A handoff that carries the
/// archive keeps an earlier process's origin. Pid plus clock does not repeat
/// in practice.
pub(crate) fn alt_archive_origin() -> u64 {
    static ORIGIN: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *ORIGIN.get_or_init(|| {
        let nanos = aterm_time::since_epoch()
            .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));
        let origin = (u64::from(std::process::id()) << 32) ^ nanos;
        origin.max(1)
    })
}

/// Estimated byte target for a new live session's in-memory grid ring, which
/// holds recent scrollback uncompressed; older lines tier into the store.
///
/// A ring row is allocated at full grid width, so each line costs about
/// `cols × size_of::<Cell>()` + 45 B regardless of content; a byte target keeps
/// wide windows from spending tens of MB per tab on uncompressed history. A
/// smaller ring pushes more rows through LZ4 (on the compression worker, else
/// the reader's inline drain). A startup estimate, not a hard limit: allocator
/// slack is not modelled, the 512-line floor can exceed it at very wide widths,
/// resizing keeps the chosen capacity, and checkpoint adoption uses
/// `restore_checkpoint`'s own 1000-line ring.
const LIVE_SCROLLBACK_RING_BYTES: usize = 4 * 1024 * 1024;

/// Upper bound on the ring's line capacity.
const LIVE_SCROLLBACK_RING_LINES_MAX: usize = 10_000;

/// A floor so a very wide window still keeps an instant-scrollback ring worth
/// having (512 lines is ~10 screens at 50 rows).
const LIVE_SCROLLBACK_RING_LINES_MIN: usize = 512;

/// The initial ring's line cap for a window `cols` wide, estimated from
/// [`LIVE_SCROLLBACK_RING_BYTES`].
fn live_scrollback_ring_lines(cols: u16) -> usize {
    // `size_of::<Cell>()` is 8; ~45 B of Row/ring overhead per line (8·cols + 45).
    let per_line = usize::from(cols)
        .saturating_mul(std::mem::size_of::<aterm_core::grid::Cell>())
        .saturating_add(45)
        .max(1);
    (LIVE_SCROLLBACK_RING_BYTES / per_line).clamp(
        LIVE_SCROLLBACK_RING_LINES_MIN,
        LIVE_SCROLLBACK_RING_LINES_MAX,
    )
}

/// One OSC 52 write, routed by [`route_osc52_write`]: the text bound for the
/// system clipboard and/or the X11 PRIMARY selection. `None` leaves that
/// destination untouched, so a `'p'`-only set never clobbers the clipboard and
/// vice versa.
struct ClipWrite {
    clipboard: Option<Arc<str>>,
    primary: Option<Arc<str>>,
}

impl ClipWrite {
    /// Merge a newer last-writer-wins update without disturbing a destination
    /// the newer OSC 52 command did not name.
    fn merge_latest(&mut self, newer: Self) {
        if newer.clipboard.is_some() {
            self.clipboard = newer.clipboard;
        }
        if newer.primary.is_some() {
            self.primary = newer.primary;
        }
    }
}

/// Constant-memory handoff from the terminal callback to the blocking
/// platform-clipboard worker. There is exactly one pending [`ClipWrite`]; a
/// producer replaces each named destination in that slot instead of enqueueing
/// another arbitrarily large OSC 52 payload.
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
    /// Publish without waiting for the worker. The only contended section is a
    /// pointer swap under the mailbox lock; platform I/O never holds it.
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

/// Route an OSC 52 selection list onto the two real destinations.
///
/// * `'p'` (Primary) goes to the PRIMARY selection when `primary_supported`
///   (X11); elsewhere PRIMARY does not exist and `'p'` falls back to the
///   clipboard, which OSC 52 clients on macOS/Windows rely on.
/// * Everything else (`'c'`, `'s'`, `'q'`, the cut buffers, and the empty list,
///   xterm's default) goes to the system clipboard.
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

/// The slot an OSC 52 query answers from, the read-side mirror of
/// [`route_osc52_write`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(any(target_os = "linux", test))]
enum QuerySource {
    Clipboard,
    Primary,
}

/// Route an OSC 52 query's selection list onto the slot it reads, mirroring
/// [`route_osc52_write`] so set-then-query is coherent: a `'p'`-only query reads
/// PRIMARY (X11); `'c'` and friends and the empty list read the clipboard. One
/// query yields one response, so a mixed `"cp"` list answers from the clipboard
/// (its Set wrote both). Off X11 `'p'` folds into the clipboard.
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

/// What aterm, running outside any sandbox, does at one session's program's
/// request, decided once, at spawn, by [`host_reach`] from the containment
/// mode its shell runs under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HostReach {
    /// The platform. OSC 52 set and clear (and the OSC 1337 copies, which take
    /// the same authority) are authorized, and a query is answered when
    /// `allow_osc52_query` authorizes it. OSC 9/99/777 reach the desktop
    /// notifier when `allow_notifications` admits them. Kitty's file,
    /// temporary-file and shared-memory mediums are read when
    /// `allow_kitty_file_transfer` opts in.
    Platform,
    /// Nothing. No clipboard write or query is authorized, and no clipboard
    /// transport, notification callback or Kitty medium resolver is
    /// installed. A grant made later (a config reload's `allow_osc52_query` or
    /// `allow_notifications`, a policy rule, a carried checkpoint's bits)
    /// therefore has nothing behind it, and a Kitty file or shared-memory
    /// transmission is answered as a disabled medium, whatever it names.
    Refused,
}

/// What aterm does for the program of a session spawned under `mode`.
///
/// Fail closed: aterm, unsandboxed, brokers nothing to a session spawned under
/// `Containment`. Its profile refuses the program the pasteboard
/// (`docs/AUDIT-boundaries-2026-10-09.md` L1), the credential and private-data
/// stores, and every mach service but the user and group queries, and each
/// broker would hand it what the profile refuses:
///
/// - OSC 52: setting the pasteboard by printing (a planted command waiting for
///   the person's next paste) and, under `allow_osc52_query`, reading it back.
/// - Kitty `t=f`/`t=t`/`t=s` under `allow_kitty_file_transfer`: aterm reads any
///   path the program names, so the reply tells whether a refused file exists
///   and its size, its bytes are drawn into the tab, and a `t=t` deletes a
///   temporary file as aterm.
/// - OSC 9/99/777 under `allow_notifications`: the program's text posted as a
///   desktop notification, through a notifier the profile refuses it.
///
/// Decided once per session, where each broker is installed; the handlers carry
/// no second check. What the person does (a link click, a paste, a drop) is not
/// brokered at the program's request and keeps working. Safety keeps the
/// platform: it has no OS sandbox, so its program could do each itself. A mode
/// this build does not know is refused.
fn host_reach(mode: aterm_containment::ContainmentMode) -> HostReach {
    use aterm_containment::ContainmentMode as Cm;
    match mode {
        Cm::User | Cm::Safety => HostReach::Platform,
        _ => HostReach::Refused,
    }
}

/// OSC 52 clipboard for one session: what it may reach is `reach`
/// ([`host_reach`]). Where that is the platform clipboard, writes are
/// authorized (pbcopy / PRIMARY-own on a dedicated thread, so the blocking
/// platform write never runs under the Terminal lock) and queries denied unless
/// configured. Each tab gets its own authorization and callback, so a
/// background tab's yank reaches the platform. Targets are routed by
/// [`route_osc52_write`] and queries by [`route_osc52_query`].
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

/// What this process's sessions reach on their programs' behalf
/// ([`host_reach`] for the containment mode they run under).
pub(crate) fn session_host_reach() -> HostReach {
    host_reach(aterm_containment::mode_or_containment())
}

/// A session's host wiring (design `docs/DESIGN-warm-spare-2026-10-16.md`
/// §5.2): the clipboard, notifications, Kitty's file mediums, the bell and
/// (Linux) window operations, each counting what it delivers in `witness`.
/// Made once at spawn for every session but a spare, and at a spare's
/// adoption, so a cold tab and an adopted one end with identical wiring. The
/// configured OSC 52 query is granted again here: a spare had it revoked.
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
    // Kitty file/shm transfer mediums — only when the user opted in (default off).
    if factory.allow_kitty_file_transfer {
        configure_kitty_file_transfer(term, reach);
    }
    configure_bell(term, proxy, id, window, witness);
    // XTWINOPS host transport (Linux): the engine gates on `allow_window_ops`
    // in-core; without this callback an authorized `CSI 9;1t` was a no-op.
    #[cfg(target_os = "linux")]
    configure_window_ops(term, proxy, id, window);
}

/// The platform write behind [`configure_clipboard`]'s worker thread.
fn deliver_clip_write(write: ClipWrite) {
    if let Some(text) = write.clipboard {
        control::pbcopy(&text);
    }
    #[cfg(target_os = "linux")]
    if let Some(text) = write.primary {
        control::primary_set(&text);
    }
}

/// [`configure_clipboard`] with the platform write as a parameter, so a test
/// can watch what reaches it.
fn install_clipboard(
    term: &Arc<Mutex<Terminal>>,
    reach: HostReach,
    deliver: impl FnMut(ClipWrite) + Send + 'static,
) {
    if reach == HostReach::Refused {
        // No worker, no callback: `deliver` drops here, unused.
        let mut t = term_lock(term);
        t.revoke_clipboard_access(ClipboardAccess::Write);
        t.revoke_clipboard_access(ClipboardAccess::Query);
        return;
    }
    let (clip_tx, clip_rx) = clip_mailbox();
    // The worker starts with the session's first clipboard write, not with the
    // session: OSC 52 is rare, and a parked thread per tab is a stack every
    // later fork of this process copies into the child's VM map (a new tab's
    // fork grew about 10 µs per open tab, measured 2026-10-17). One that cannot
    // start leaves the writes unsent, as a refused reach does.
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
                // The mailbox keeps at most one pending update while a platform
                // call is blocked. Each selection is last-writer-wins, so a
                // flood collapses to the latest value per destination without
                // retaining every payload.
                while let Some(write) = clip_rx.recv() {
                    deliver(write);
                }
            });
    };
    let mut t = term_lock(term);
    t.authorize_clipboard_access(ClipboardAccess::Write);
    // Off X11 'p' folds into the clipboard, so the routed `primary` half is
    // always `None` and the PRIMARY write compiles out.
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
        // Reached only through a minted `ClipboardQueryCapability`
        // (`allow_osc52_query = true`, default false, or a policy rule deciding
        // allow); the response still passes the response rate/budget gates.
        // An authorized query always gets an answer: an empty clipboard is an
        // empty reply, since silence is indistinguishable from denial.
        ClipboardOperation::Query { selections } => {
            #[cfg(not(target_os = "linux"))]
            {
                // Off X11 every target (the 'p' spelling included) folds into
                // the one pasteboard, mirroring the write fallback above. The
                // pasteboard read can block (a Universal Clipboard fetch, a busy
                // lazy provider), so it is never done here, under the terminal
                // lock: the reader did it before the hold that feeds the query
                // (`host_io_prefetch`), and a query it did not read is unanswered.
                let _ = &selections;
                crate::host_io_prefetch::take_clipboard_query()
            }
            #[cfg(target_os = "linux")]
            {
                // X11: only the non-blocking own-selection read; a foreign owner
                // would need a blocking round-trip inside the terminal lock.
                // The slot follows [`route_osc52_query`], so set-then-query
                // answers from the same buffer for both targets.
                match route_osc52_query(&selections, primary_supported) {
                    QuerySource::Primary => crate::control::primary_get_owned(),
                    QuerySource::Clipboard => crate::control::pbpaste_owned(),
                }
            }
        }
    });
}

/// Desktop notifications for one session (OSC 9 / OSC 99 kitty / OSC 777). Each
/// tab registers its own callbacks, so a background tab's notification still
/// surfaces. The callbacks run on the reader thread under the Terminal lock, so
/// they only `try_send` onto the shared bounded channel
/// (`notify::NOTIFY_QUEUE_CAP`), dropping on `Full`; the notifier and
/// focus-aware suppression run on `notify`'s delivery thread. A session whose
/// `reach` ([`host_reach`]) is refused gets no callback, so no
/// `allow_notifications` grant reaches the notifier from it.
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
    // No authorization here: the engine's OSC 9/99/777 handlers check
    // `modes.allow_notifications` (from config, default off) before invoking
    // these callbacks.
    // OSC 9 / 777: a bare body string, no title.
    let tx = notify_tx.clone();
    let w = Arc::clone(witness);
    t.set_notification_callback(move |body| {
        w.host_effects.fetch_add(1, Ordering::Relaxed);
        // try_send (never send): on a full BOUNDED queue this DROPS the message
        // instead of blocking the reader thread — bounding queue memory under a flood.
        let _ = tx.try_send(notify::NotifyMsg::program(id, None, body.to_string()));
    });
    // OSC 99 (kitty): structured title + body. Drop empty notifications
    // (close/update control frames with no content) rather than popping a
    // blank toast.
    let tx = notify_tx.clone();
    let w = Arc::clone(witness);
    t.set_advanced_notification_callback(move |n| {
        if !n.has_content() {
            return;
        }
        w.host_effects.fetch_add(1, Ordering::Relaxed);
        // try_send (never send): drop on a full BOUNDED queue rather than block.
        let _ = tx.try_send(notify::NotifyMsg::program(
            id,
            n.title,
            n.body.unwrap_or_default(),
        ));
    });
}

/// Raised by any session's engine at its FIRST refused palette set (OSC 4,
/// numeric OSC 21, under `allow_palette_reconfigure = false`); taken by the
/// event loop's `about_to_wait` ([`take_palette_refused`]), which files the one
/// digest line (`App::note_palette_refused`). The set arrives in a PTY chunk,
/// whose own output wake runs that turn, so no wake of its own is needed.
static PALETTE_REFUSED: AtomicBool = AtomicBool::new(false);

/// Whether a session refused a palette set since the last call.
pub(crate) fn take_palette_refused() -> bool {
    PALETTE_REFUSED.load(Ordering::Relaxed) && PALETTE_REFUSED.swap(false, Ordering::AcqRel)
}

/// Tell the event loop about this session's first refused palette set. Runs
/// on the reader thread under the Terminal lock, so it only raises a flag.
fn configure_palette_refusal(term: &Arc<Mutex<Terminal>>) {
    term_lock(term).set_palette_refused_callback(|| {
        PALETTE_REFUSED.store(true, Ordering::Release);
    });
}

/// BEL → `Wake::Bell{id}` for one session. Fires inside `process()` on this tab's
/// reader thread, under the Terminal lock, so it only wakes the UI; the main thread
/// beeps/flashes.
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

/// XTWINOPS to `Wake::WindowOp` for one session (Linux). The engine's `CSI t`
/// dispatch holds the security policy (nothing runs unless `allow_window_ops =
/// true` minted a capability; moves are denied; resizes arrive clamped), so this
/// callback is transport only: it runs on the reader thread under the Terminal
/// lock and posts the operation to the main thread (`App::on_window_op`).
///
/// Report operations return `None`: an async wake cannot carry a synchronous
/// reply. The engine's in-core fallbacks answer text-grid size, title/icon
/// label and the pixel pair (from `App::sync_cell_pixel_size`'s metrics);
/// window/screen position and screen size stay unanswered.
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
        // Manipulations answer no bytes; reports keep the engine's fallbacks.
        None
    });
}

/// The pure half of [`configure_window_ops`]: manipulations become the
/// [`Wake::WindowOp`] the main thread applies, stamped with the requesting
/// session and its spawn-time owner window; reports and unknown operations
/// return `None`. Split out because an `EventLoopProxy` cannot exist headless.
#[cfg(target_os = "linux")]
pub(crate) fn window_op_wake(
    op: aterm_types::WindowOperation,
    session: u64,
    window: WindowId,
) -> Option<Wake> {
    use aterm_types::WindowOperation as W;
    match op {
        // Manipulations: applied asynchronously by the main thread; none of
        // these answers bytes, so posting alone is complete.
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
        // Reports and any future operation: unanswered here (the engine's in-core
        // fallbacks answer what they can). `MoveWindow` is denied in-core.
        _ => None,
    }
}

/// Maximum bytes a Kitty non-direct medium may supply (matches the engine's
/// `MAX_KITTY_IMAGE_BYTES`): bounds both a huge file and a huge shm object.
pub(crate) const MAX_KITTY_MEDIUM_BYTES: u64 = 4 * 1024 * 1024;

/// Install the Kitty non-direct-medium resolver for one session (opt-in via
/// `allow_kitty_file_transfer`) whose `reach` ([`host_reach`]) is the platform;
/// a refused session gets none, so the engine answers its file and
/// shared-memory transmissions as a disabled medium and touches nothing they
/// name. The engine hands us `(medium, path/name)`; we do the I/O under a
/// fail-closed policy and return the raw image bytes:
/// - `t=f` (file): read a regular file, size-capped.
/// - `t=t` (temp file): read it, then delete it — only as the protocol allows
///   ([`consume_kitty_temp_file`]).
/// - `t=s` (shared memory): `shm_open(O_RDONLY)` + `mmap`, copy out, then
///   `shm_unlink`.
///
/// A `t=t` file or `t=s` object is consumed only once the engine has taken
/// what was read for it ([`consume_kitty_medium`], after the hold): one the
/// engine refuses before asking (a size it does not load, a resolver gone, the
/// string budget) is left where the program put it.
///
/// Readability is bounded by our uid and size by the cap. The engine never
/// touches the filesystem or shm itself.
///
/// A file read can block (an evicted iCloud file downloading, a stalled network
/// volume), and the resolver runs inside `process()`, under the terminal lock:
/// so the reads are done by the reader before the hold that feeds what names them
/// ([`crate::host_io_prefetch`], through [`read_kitty_medium`]), and the resolver
/// only takes what was read. A medium the reader did not read is refused.
fn configure_kitty_file_transfer(term: &Arc<Mutex<Terminal>>, reach: HostReach) {
    if reach == HostReach::Refused {
        return;
    }
    term_lock(term).set_kitty_file_resolver(crate::host_io_prefetch::take_kitty_medium);
}

/// One Kitty non-direct medium's bytes, read under [`configure_kitty_file_transfer`]'s
/// policy: `t=f` a regular file, `t=t` a temporary file, `t=s` a shared-memory
/// object. It consumes nothing ([`consume_kitty_medium`] does, once the engine has
/// taken the bytes). Direct is the engine's own; any future medium fails closed.
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

/// Consume a medium the engine took ([`read_kitty_medium`]'s read of it, `read`
/// whether that read returned bytes): a `t=t` file read is deleted as the protocol
/// allows ([`consume_kitty_temp_file`]), and a `t=s` object is unlinked, read or not
/// (the client handed it over). A `t=f` file is never the terminal's to remove.
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

/// Read a Kitty `t=f` file: a regular file, at most [`MAX_KITTY_MEDIUM_BYTES`].
fn read_kitty_file(path: &Path) -> Option<Vec<u8>> {
    // Open once and validate the handle (fstat), never the path, so a
    // same-uid writer cannot swap in a FIFO or grow the file between
    // check and read. O_NONBLOCK keeps a writerless FIFO from blocking the
    // open; it is a no-op for regular files.
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
        return None; // grew past the cap mid-read
    }
    Some(bytes)
}

/// The string the Kitty graphics protocol requires in the path of a `t=t`
/// temporary file before the terminal may delete it.
const KITTY_TEMP_FILE_MARKER: &str = "tty-graphics-protocol";

/// Delete a Kitty `t=t` temporary file the engine took (read as `t=f` reads any
/// file), only where the protocol's security rule allows: in a known temporary
/// directory ([`kitty_temp_dirs`]), with [`KITTY_TEMP_FILE_MARKER`] in its
/// path. Any other file is kept, as kitty keeps it.
///
/// The path comes from the program in the tab, which may be a remote host or
/// a file being `cat`, so without this rule a transmission naming any file
/// the user can write deletes it.
fn consume_kitty_temp_file(path: &Path) {
    consume_kitty_temp_file_with(path, || {});
}

/// [`consume_kitty_temp_file`], running `before_unlink` once the entry has been
/// judged deletable and just before it is unlinked: the window a test swaps a
/// directory in.
fn consume_kitty_temp_file_with(path: &Path, before_unlink: impl FnOnce()) {
    if let (Some(dir), Some(name)) = (path.parent(), path.file_name()) {
        consume_kitty_temp_entry(dir, name, before_unlink); // the client's temp file
    }
}

/// Whether the protocol lets a `t=t` entry at `entry` (its directory resolved)
/// be deleted: [`KITTY_TEMP_FILE_MARKER`] in its path, under a
/// [`kitty_temp_dirs`] directory.
fn kitty_temp_entry_deletable(entry: &Path) -> bool {
    entry.to_string_lossy().contains(KITTY_TEMP_FILE_MARKER)
        && kitty_temp_dirs().iter().any(|tmp| entry.starts_with(tmp))
}

/// Delete `name` in `dir` if [`kitty_temp_entry_deletable`] allows it, judging
/// and unlinking in one held directory.
///
/// The canonical `dir` is opened component by component from `/` with
/// `O_DIRECTORY | O_NOFOLLOW`, so a symlink anywhere in it refuses the open; the
/// rule is judged on that path and the entry unlinked relative to the held
/// descriptor, never by path, and only if it is a regular file. A program that
/// can write the temporary directories therefore cannot swap a component for a
/// symlink between judgement and unlink and have aterm remove a file elsewhere.
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

/// Off Unix the entry is removed by path. Containment, the one mode whose
/// program has fewer filesystem rights than aterm, exists only on macOS, so
/// here nothing on the other side of the race lacks the right to delete the
/// file itself.
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

/// The known temporary directories a Kitty `t=t` file may be deleted from,
/// resolved: `/tmp`, `/dev/shm`, `$TMPDIR` and the platform's own.
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

/// Read a POSIX shared-memory object by name (`shm_open` + `mmap`, size-capped),
/// leaving it in place ([`unlink_posix_shm`] removes it once the engine took it).
/// Returns `None` on any error. `unix`-only; a no-op stub elsewhere.
#[cfg(unix)]
fn read_posix_shm(name: &str) -> Option<Vec<u8>> {
    let cname = std::ffi::CString::new(name).ok()?;
    // SAFETY: `cname` is a valid NUL-terminated C string; `shm_open` with O_RDONLY
    // either returns a valid fd or -1, which we check.
    let fd = unsafe { libc::shm_open(cname.as_ptr(), libc::O_RDONLY, 0) };
    if fd < 0 {
        return None;
    }
    // Ensure the fd + mapping are always released, and the object unlinked.
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

/// Remove a POSIX shared-memory object's name: the Kitty client handed the object
/// to the terminal, which consumes it once the engine took it, whether or not its
/// read succeeded. `unix`-only; a no-op elsewhere.
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

/// Bound for the temporal-recorder writer queue (B.9): a full queue drops the
/// slice instead of growing without bound while the writer thread is behind
/// under a flood, and the drop is a counted break the reader closes with a
/// keyframe (`crate::temporal::SpineMark`). See [`spawn_temporal_writer`].
const TEMPORAL_QUEUE_CAP: usize = 1024;

/// Temporal recorder writer thread: the reader hands its spine messages here
/// lock-free, so the spine append and a re-keyframe's replay never run on the
/// reader's hot path or under `term_lock`. FIFO preserves order; an idle
/// terminal parks on `recv()`. The queue is bounded ([`TEMPORAL_QUEUE_CAP`]); a
/// full queue drops the slice, a counted break.
/// Each message is one `crate::temporal::write`: the append under the recorder
/// lock, and any re-keyframe's replay after releasing it.
fn spawn_temporal_writer(
    temporal: Arc<std::sync::Mutex<crate::temporal::TemporalRecorder>>,
) -> Result<(crate::temporal::TemporalTx, std::thread::JoinHandle<()>), String> {
    let (temporal_tx, temporal_rx) = crate::temporal::queue(TEMPORAL_QUEUE_CAP);
    let join = crate::qos::spawn_lane_thread(
        "aterm-temporal-writer",
        crate::qos::Role::Responsive,
        move || {
            // The main thread waits on this worker (`park_reader` spins on
            // `is_finished()` under the handoff deadline), and control lanes take
            // the `temporal` mutex it holds per message, so it runs `Responsive`.
            while let Some(msg) = temporal_rx.recv() {
                crate::temporal::write(&temporal, msg);
            }
        },
    )
    .map_err(|error| format!("spawn temporal writer: {error}"))?;
    // The join handle lets `park_reader` drain this writer before a re-attach
    // spawns a fresh one into the same recorder, so events never reorder across
    // attaches. The `recv` loop ends once every sender drops (the reader's, at
    // thread exit), so the join is bounded.
    Ok((temporal_tx, join))
}

/// PTY query-reply writer thread: the reader hands DA/DSR/CPR replies
/// (`take_response()`), and behind them the reports the engine sent unasked
/// (`take_reports()`, handed on by their [`ReportRoute`]), here lock-free and
/// never writes them inline. An inline
/// write to the PTY input can block when the child is not reading input, which
/// would stop the reader draining output and deadlock the session (on Windows
/// it would also hang `ClosePseudoConsole`, which drains through the reader).
/// FIFO preserves reply order, and the sink's whole-frame lock serializes it
/// with other writers. The thread ends when every sender is gone: the reader's
/// at EOF, and the spill arranger's when the sink's last strong clone drops.
///
/// The reply queue is bounded in count (`sync_channel`) and bytes (each reply
/// holds its bytes against [`aterm_session::sink::REPLY_BUDGET_BYTES`] via
/// [`ReplyLane::submit`]), so a child that floods queries without reading its
/// stdin cannot pile replies up. A reply that finds either full is dropped and
/// counted (`reply_dropped` under `metrics percentiles`). Sized so no real
/// capability-probe burst drops.
const REPLY_QUEUE_CAP: usize = 1024;

/// What the reply-writer thread is asked to do. Terminal replies are its job;
/// arranging the sink's spill drainer is the second, so a keystroke that
/// concedes the fd lock never `dup`s and `pthread_create`s on the UI thread.
pub(crate) enum ReplyJob {
    /// A DA/DSR/CPR/kitty-query reply from the parser, written in order, with
    /// its bytes held against the sink's reply budget until it is written.
    Bytes(std::sync::Arc<[u8]>, aterm_session::sink::ReplyPermit),
    /// A report the engine sent unasked (`Terminal::take_reports`: a resize's
    /// DEC 2048 report, a DSR 997 push), handed on by its [`ReportRoute`].
    Report(std::sync::Arc<[u8]>, aterm_session::sink::ReplyPermit),
    /// A non-parking writer spilled with no drainer live: spawn it from here.
    ArrangeSpillDrainer,
}

/// The reader's end of the reply lane: hands a reply to the writer thread
/// without blocking. It holds the sink's budget, never the sink: the reader must
/// not keep its own session's master alive or drop its last clone (on Windows
/// closing the pseudo console drains through this reader).
#[derive(Debug)]
pub(crate) struct ReplyLane {
    tx: std::sync::mpsc::SyncSender<ReplyJob>,
    budget: aterm_session::sink::ReplyBudget,
}

impl ReplyLane {
    /// Queue one reply: reserve its bytes against the sink's reply budget, then
    /// `try_send`. A full budget, a full queue or a closing session drops the
    /// reply, counted as `reply_dropped`. `true` when queued.
    pub(crate) fn submit(&self, reply: std::sync::Arc<[u8]>) -> bool {
        self.queue(reply, ReplyJob::Bytes)
    }

    /// [`Self::submit`] for a report the engine sent unasked, which the writer
    /// hands to its [`ReportRoute`] instead of writing it as a reply. Behind the
    /// replies of its burst, on the same FIFO and budget.
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

/// Where the reply writer sends a report the engine sent unasked
/// ([`ReplyJob::Report`]). On Windows: the input's way, the session's ordered
/// input writer, as a mouse or focus report goes
/// (`app_input::write_ui_report`). A report is input the program asked for in
/// advance, so the ConPTY resize guard must lead it (`aterm_pty`'s
/// `resize_guard`: the MSYS2 runtime drops the record after a size event, and
/// a resize's DEC 2048 report was that record, its ESC lost, `[48;30;100;…t`
/// typed into vim), and the guard's OpenConsole client may wait on the console
/// host, which may be waiting for a reply queued behind the report on the
/// reply writer's FIFO: the input writer waits, the reply writer never does.
/// Elsewhere nothing guards a PTY's input, and a report is written as a reply.
#[derive(Clone, Default)]
pub(crate) struct ReportRoute {
    /// The session's engine (weakly: this writer must not keep a closed
    /// session's engine alive) and its mode mirror, which the input route
    /// takes.
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

    /// Hand `report` to the input route; `false` when there is none (off
    /// Windows, a route made by `Default`, an engine already gone) and the
    /// caller writes it as a reply.
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
    // The spill-arranger `poke` installed below is a sender of this thread's
    // channel living inside the sink's `Shared`, so the thread holds the sink
    // only weakly: a strong clone would form a cycle in which `recv()` never
    // disconnects and every closed session leaks its writer thread, spill state
    // and PTY master. Pinned by `reply_writer_lifetime_tests`.
    let weak = Arc::downgrade(&sink);
    let join = spawn_reply_writer_thread(move || {
        // Holds `Shared.lock`, the mutex the keystroke write spins on, for every
        // reply it writes, so it must not run below the UI thread (the qos.rs
        // floor rule).
        crate::qos::set_self(crate::qos::Role::Interactive);
        while let Ok(job) = reply_rx.recv() {
            // Every sender is held by a holder of a strong clone, so a failed
            // upgrade means that holder was the last and dropped it after
            // sending: the master is already closed, nothing is left to write.
            let Some(sink) = weak.upgrade() else { break };
            match job {
                // The same whole-frame NON-PARKING write the keystroke path
                // uses: on the O_NONBLOCK master its hold is ONE write(2)
                // (no poll under the lock); a full tty spills the tail to the
                // ordered drainer instead of parking here with the lock held.
                // Oversized replies fall back to the blocking path inside.
                // A reply, not driven input: a shell that was only answered
                // has still not been used (`ShellStart::input`), and on
                // Windows it passes the ConPTY resize guard, which stays owed
                // to the next input (`aterm_pty::as_reply`: OpenConsole waits
                // for its own cursor query's answer after a resize).
                ReplyJob::Bytes(resp, permit) => {
                    let _ = aterm_pty::as_reply(|| sink.write_reply(&resp));
                    // Written (or failed with a dying session): the reply
                    // budget has these bytes back.
                    drop(permit);
                }
                // Handed on without waiting for it to be written (`ReportRoute`),
                // or, with no route, written as a reply is.
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
    // The UI thread's keystroke write concedes the fd lock to whoever holds it;
    // this thread arranges the drainer, so a concession is a VecDeque push plus
    // a `try_send`, never a thread creation on the event loop. A full queue
    // drops the poke and the next conceding write re-pokes. The slot is
    // first-install-wins for the sink's life, so it is installed only once the
    // thread exists: a poke into a receiver-less channel would hold it and
    // keystrokes would never be delivered.
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

/// Test seam: a real reply writer over `sink`, for the egress-contract tests
/// that drive replies and pastes through one session's sink.
#[cfg(all(test, unix))]
pub(crate) fn spawn_reply_writer_for_test(
    sink: Arc<SinkWriter>,
) -> (ReplyLane, std::thread::JoinHandle<()>) {
    spawn_reply_writer(sink, ReportRoute::default()).expect("spawn the reply writer")
}

/// The reply writer's one thread creation, behind a seam
/// `reply_writer_lifetime_tests` can fail on demand: thread exhaustion (EAGAIN)
/// is the failure `spawn_reply_writer` must leave no trace of, and a test
/// cannot provoke it reliably.
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
    /// Armed by a test on its thread: the next `spawn_reply_writer_thread` on
    /// that thread fails instead of spawning (one-shot, consumed by the check).
    static REPLY_WRITER_SPAWN_FAULT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn take_injected_reply_writer_spawn_fault() -> bool {
    REPLY_WRITER_SPAWN_FAULT.with(|f| f.replace(false))
}

/// Shipping builds have no fault seam: the spawn is always attempted.
#[cfg(not(test))]
fn take_injected_reply_writer_spawn_fault() -> bool {
    false
}

/// Where the reply writer puts a report the engine sent unasked, on Windows.
#[cfg(all(test, windows))]
mod report_route_tests {
    use super::{ReportRoute, new_live_terminal, spawn_reply_writer};
    use aterm_session::sink::SinkWriter;
    use aterm_spec::wait::{MUST_HAPPEN, within};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// A sink on a key no ConPTY session has: its writes fail, but every
    /// attempt is counted first, which is what these tests read.
    fn sink() -> Arc<SinkWriter> {
        Arc::new(SinkWriter::new(0x7fff_0f0f))
    }

    /// With its session's route a report goes the input's way, through the
    /// ordered input writer, as a focus report does: it is input someone drove
    /// (`driven_input` moves), so the ConPTY resize guard leads it. Written as
    /// a reply (the old way, which left the record the MSYS2 runtime drops after
    /// a size event to the report's ESC, `[48;30;100;600;900t` typed into vim)
    /// it never moves `driven_input`.
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

    /// Negative control: a reply moves the input epoch and not `driven_input`,
    /// and so does a report with no route (a route made by `Default`).
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
    //! The reply writer's spill-arranger duty must not pin the session: the
    //! arranger's sender lives inside the sink, so a writer holding the sink
    //! strongly would keep its channel, spill state and PTY master alive.
    use super::{ReportRoute, spawn_reply_writer};
    use aterm_session::sink::SinkWriter;
    use std::io::Read as _;
    use std::os::unix::net::UnixStream;
    use std::sync::Arc;
    use std::time::Duration;

    // A hang detector for things that must happen (a reply, EOF, a writer
    // exiting), never a latency budget.
    use aterm_spec::wait::MUST_HAPPEN;

    /// A master-owning sink over one end of a socketpair; the peer sees what it
    /// writes and EOF when the sink's `OwnedFd` closes. The sink's end is
    /// `O_NONBLOCK`, as a session's master is, so a frame larger than the
    /// socket's buffers spills its tail instead of parking the writer.
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

    /// Larger than any socketpair's buffers, under the spill's cap.
    const SPILL_FRAME: usize = 1 << 20;

    /// The writer carrying the sink's arranger duty is ALIVE, as a fact: a
    /// frame larger than the socket's buffers spills its tail, and a spill's
    /// drainer is arranged only by a poke to that writer
    /// (`install_spill_arranger`). Every byte reaching the peer is that writer
    /// serving the sink; a dead one strands the tail, and the read gives up at
    /// [`MUST_HAPPEN`].
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

    /// The class the writer runs at, asked of the kernel: the `Role::Interactive`
    /// line in [`spawn_reply_writer`]'s closure is the only thing setting it.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_reply_writer_runs_at_the_class_the_keystroke_path_needs() {
        use std::os::unix::thread::JoinHandleExt as _;
        let (sink, mut peer) = sink_and_peer();
        let (reply_tx, join) =
            spawn_reply_writer(sink.clone(), ReportRoute::default()).expect("spawn");
        let thread = join.as_pthread_t() as libc::pthread_t;
        // The class is the inherited default until the closure's first statement
        // runs, which comes before the writer's first `recv()`: a reply written
        // to the peer proves it ran.
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
        // The re-attach's writer holds nothing but its reader's sender: gone now.
        wait_finished(&second_join, "the re-attached reply writer");
        // The first carries the arranger duty for as long as the sink lives…
        spill_is_delivered(&sink, &mut peer);
        // ...and no longer, so the master still closes with the session.
        drop(sink);
        wait_finished(&first_join, "the first reply writer");
        let n = peer
            .read(&mut [0u8; 1])
            .expect("EOF, not a read timeout: the master fd must have closed");
        assert_eq!(n, 0, "EOF");
    }

    /// The arranger slot is first-install-wins for the sink's life, so it may
    /// only hold a sender whose writer thread exists. After a failed spawn and
    /// a retried attach, the retry's writer must be the arranger worker (it
    /// outlives its reader's sender while the sink lives).
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
        // The retry — a fresh writer into the same sink.
        let (retry_tx, retry_join) =
            spawn_reply_writer(sink.clone(), ReportRoute::default()).expect("the retry spawns");
        assert!(retry_tx.submit(Arc::from(&b"ok"[..])), "queued");
        let mut buf = [0u8; 2];
        peer.read_exact(&mut buf)
            .expect("the retry's writer writes");
        assert_eq!(&buf, b"ok");
        drop(retry_tx);
        // The retried attach's writer carries the arranger duty: a failed spawn
        // may not leave its dead sender in the slot.
        spill_is_delivered(&sink, &mut peer);
        drop(sink);
        wait_finished(&retry_join, "the retried reply writer");
        let n = peer
            .read(&mut buf)
            .expect("EOF, not a read timeout: the master fd must have closed");
        assert_eq!(n, 0, "EOF");
    }
}

// Off-thread tier-compression worker tuning.
//
// A capacity-1 signal channel coalesces reader notifications: a token already
// queued means "a drain is pending", so a `try_send` drop is harmless.
const COMPRESS_SIGNAL_CAP: usize = 1;
/// The reader signals the worker once its deferred lazy backlog reaches this many
/// lines — below the 1000-line inline drain threshold, so the worker starts
/// promoting before the backlog grows large.
const COMPRESS_SIGNAL_AT: usize = 900;
/// Lines the worker promotes per term-lock hold: small enough that the render
/// thread and PTY reader wait at most this batch's LZ4 cost (a couple of block
/// compressions, sub-frame) per hold.
const COMPRESS_BUDGET: usize = 256;
/// The worker stops draining once the backlog falls to/below this small
/// amortization window, so it never thrashes the lock on the last few lines.
const COMPRESS_LOW_WATER: usize = 256;
/// Signals arriving within this window of each other mean the reader is
/// mid-flood; the worker defers promotion until the stream goes quiet, unless
/// pressure relief is due before the memory cap cuts the staged history.
const COMPRESS_QUIET_WINDOW: std::time::Duration = std::time::Duration::from_millis(50);
/// Mid-flood, promote at most one bounded batch this often: forward progress on
/// the backlog without measurably contending the flood's term-lock holds.
const COMPRESS_TRICKLE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// Per-session off-thread tier-compression worker. Parks on `recv()`; on each
/// signal it promotes the engine's deferred lazy backlog into the compressed
/// tiers in [`COMPRESS_BUDGET`]-line batches, releasing the term lock and
/// yielding between batches, so LZ4 work stays off the reader's drain path.
///
/// Returns `None` if the thread could not be spawned; the caller then leaves
/// the offload inactive and the reader drains inline at the 1000-line
/// threshold. Ends when the reader drops the returned sender.
fn spawn_compress_worker(
    term: Arc<Mutex<Terminal>>,
    ui_waiting: Arc<std::sync::atomic::AtomicU32>,
) -> Option<std::sync::mpsc::SyncSender<()>> {
    let (tx, rx) = std::sync::mpsc::sync_channel::<()>(COMPRESS_SIGNAL_CAP);
    crate::qos::spawn_lane_thread(
        "aterm-scrollback-compress",
        crate::qos::Role::Responsive,
        move || {
            // Background work that takes the `term` mutex the UI thread takes
            // on the keystroke path; being descheduled while holding it stalls
            // every waiter. `Responsive` (user-initiated) shortens that window
            // without outranking the UI thread.
            while rx.recv().is_ok() {
                // Flood gate: back-to-back signals mean the reader is mid-flood,
                // and promotion batches would contend the term lock with the PTY
                // drain. Relieve memory-bound pressure, otherwise wait for signals
                // to go quiet, with one batch per
                // `COMPRESS_TRICKLE_INTERVAL` so a perpetual flood still progresses.
                // Memory stays bounded: past the staging cap the reader drops its
                // oldest staged lines (marked in history). The trickle is skipped
                // while the reader is cutting and a batch or more arrived since
                // the last one, so a flood leaves one marker, not one per batch
                // (`Grid::trickle_lazy_bounded`).
                let mut last_trickle = std::time::Instant::now();
                relieve_flood_pressure(&term, &ui_waiting);
                while let Ok(()) = rx.recv_timeout(COMPRESS_QUIET_WINDOW) {
                    relieve_flood_pressure(&term, &ui_waiting);
                    if last_trickle.elapsed() >= COMPRESS_TRICKLE_INTERVAL {
                        term_lock(&term).trickle_lazy_bounded(COMPRESS_BUDGET);
                        last_trickle = std::time::Instant::now();
                        // Same slice-boundary handoff as the reader.
                        crate::yield_to_ui_waiter(&ui_waiting);
                    }
                }
                // Quiet or teardown: drain to the low-water mark in bounded,
                // lock-yielding batches, stopping when a batch makes no progress
                // (while the store is detached for an off-thread reflow
                // `drain_lazy_bounded` is a no-op, and waiting for low water
                // would spin the lock).
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
                    // Hand the boundary to a parked UI acquisition first, then
                    // yield to the scheduler.
                    crate::yield_to_ui_waiter(&ui_waiting);
                    std::thread::yield_now();
                }
            }
        },
    )
    .ok()
    .map(|_| tx)
}

/// Promote a memory-bound flood before the reader reaches its cut, in the
/// same bounded lock holds and UI handoffs as the quiet drain. Count the
/// excess only after the previous count was promoted, avoiding a full scan
/// for each batch. The cut-aware drain rechecks every batch: a reader that
/// opened a cut across a yield must not have its marker promoted here.
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

        // The reader can erase and refill history while the worker yields.
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

/// Buffers circulating between the gather and parse stages (unix): 4 × 64 KiB,
/// bounded memory and an allocation-free steady state.
#[cfg(unix)]
const GATHER_RING_BUFFERS: usize = 4;

/// The most output a session passes on between its shell's exit and the
/// `Wake::Exit` that reports it: far past what a PTY buffers, which is all the
/// shell itself can have left there, so only a job that keeps writing after it
/// ever meets the cap (the terminal session's own bound, `aterm-cli`'s
/// `EXIT_DRAIN_MAX`). The reader goes on reading after the report, until the
/// session's teardown stops it or the master's EOF ends it.
#[cfg(unix)]
const EXIT_DRAIN_MAX: usize = 1024 * 1024;

/// The descriptor the gather polls for the session's shell's exit
/// (`aterm_uds::exitwatch::exit_fd`): a pidfd on Linux, a kqueue on macOS.
/// `None` for a stub, a shell already reaped (its pid is free), one whose pid
/// no longer names it, and where no watch can be made; the session then ends
/// at the master's EOF alone. The watch is taken on the pid first and the
/// identity checked after, so a reissued pid is never watched as the shell.
#[cfg(unix)]
fn shell_exit_watch(session: &Session) -> Option<ShellExit> {
    let identity = session.shell_identity;
    if session.child_reaped.load(Ordering::Acquire) {
        return None;
    }
    let pid = u32::try_from(identity.pid).ok().filter(|pid| *pid > 1)?;
    let watch = aterm_uds::exitwatch::exit_fd(pid)?;
    identity.verify().then_some(ShellExit { watch, pid })
}

/// The most the gather waits, once its shell's PTY has closed or its exit
/// fired, for the kernel to retire the shell before the exit is posted: a
/// shell closes its terminal on its way out, and the kernel posts the exit a
/// moment before the process can be collected, so the exit path's one
/// non-blocking look (`App::exit_status`) finds the status — the failed-start
/// line names the real one. Paid by the gather, never the UI thread, and only
/// while the shell is still there.
#[cfg(unix)]
pub(crate) const SHELL_RETIRE_WAIT: std::time::Duration = std::time::Duration::from_millis(20);

/// A session's shell's exit watch ([`shell_exit_watch`]) and the pid it
/// watches.
#[cfg(unix)]
pub(crate) struct ShellExit {
    watch: std::os::fd::OwnedFd,
    pid: u32,
}

#[cfg(unix)]
impl ShellExit {
    fn as_raw_fd(&self) -> i32 {
        std::os::fd::AsRawFd::as_raw_fd(&self.watch)
    }

    /// Wait at most `within` for the shell to be retired: the watch fires,
    /// then a look that reaps nothing (`WNOWAIT`) finds it collectable. The
    /// session's exit path keeps the one reap, and with it the latch that
    /// stops a teardown signalling a reissued pid. A shell that is not this
    /// process's child (adopted across an update) answers no such look, and
    /// its status is its exit witness's: the wait ends at the watch.
    fn await_retired(&self, within: std::time::Duration) {
        let deadline = Instant::now() + within;
        let mut fired = libc::pollfd {
            fd: self.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ms = libc::c_int::try_from(within.as_millis()).unwrap_or(libc::c_int::MAX);
        // SAFETY: one live pollfd for the length of the call.
        if unsafe { libc::poll(&mut fired, 1, ms) } != 1 {
            return;
        }
        let Ok(pid) = libc::pid_t::try_from(self.pid) else {
            return;
        };
        loop {
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
            // SAFETY: `waitid` filled a `CLD_*` siginfo or, with nothing
            // waitable under `WNOHANG`, left it zeroed; `si_pid` is defined in
            // both.
            let collectable = rc == 0 && unsafe { info.si_pid() } == pid;
            // `rc != 0`: not this process's child to collect.
            if rc != 0 || collectable || Instant::now() >= deadline {
                return;
            }
            std::thread::yield_now();
        }
    }
}

/// Whether the parse stage's slice hold is about to wait for the term lock
/// (card engine §8's `parse_lock_waits`): a probe that takes and drops the
/// lock at once when it is free, so the hold itself stays a plain
/// [`term_lock`] binding the censuses read, and a free lock costs no clock
/// read. A lock taken between the probe and the hold goes uncounted.
fn term_busy(term: &Mutex<Terminal>) -> bool {
    matches!(term.try_lock(), Err(std::sync::TryLockError::WouldBlock))
}

/// What a slice hold that [`term_busy`] found waiting (`Some`, from just
/// before it) waited, for `metrics::note_parse_hold` (`0`: it did not).
fn parse_hold_wait(waited: Option<Instant>) -> u64 {
    waited.map_or(0, |at| {
        u64::try_from(at.elapsed().as_nanos())
            .unwrap_or(u64::MAX)
            .max(1)
    })
}

/// The slice hold's probe: a held term lock is busy, a free one is not, and
/// the probe leaves a free lock free.
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

/// [`spawn_session`]'s holder adoption for a restored engine (card engine
/// §3.8, lane A4): the foreground group's leader read from the kernel
/// (`fg_probe`, `edge_image`), its token handed to
/// `Terminal::adopt_holder_after_restore` — `drawer_exact` for a drawer whose
/// build passes Exact (`HolderImage::gate_build`) — and its image offered to
/// the timeline as a group edge. A probe that names no group adopts no
/// holder (bare). Its group is made current with the image, so the gate
/// needs no later status sweep. A carried open log reads its shell blocks
/// before the foreground move; an unopened log leaves them for registration.
/// `root_pgid` is the session's shell pid already held by its spawn. Called
/// once, after the hydrate and before the reader or registration starts.
#[cfg(unix)]
pub(crate) fn adopt_restored_holder(
    term: &Mutex<Terminal>,
    timeline: &Mutex<crate::session_timeline::SessionTimeline>,
    master: i32,
    root_pgid: i32,
    fg_probe: fn(i32) -> i32,
    edge_image: crate::holder_image::ReadImage,
) -> aterm_core::terminal::HolderAdoption {
    // A carried log may hold a command whose D is now in the engine. Read
    // its blocks before the foreground move can close that command's parent.
    // A fresh log has no registration horizon: leave its blocks unread
    // until SessionStore::register opens the session. is_open(SESSION)
    // alone only says that the log has not closed.
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
            // No carried session or run is open to close; registration's
            // first sweep still owes every block. Only publish currency now.
            timeline.note_root_group(root_pgid);
            timeline.note_foreground_group(pgid);
        }
        let _ = timeline.offer_holder_image(image, crate::holder_image::EdgeKind::Group);
        if crate::holder_image::DEV_ROWS {
            timeline.record(
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

/// The engine fence's cut (card engine §3.4 step 3), made only when it
/// touches a drawer: the incoming holder is one, or the fence has a current
/// mark to close. Every other edge costs the fence nothing.
fn note_cut(t: &mut Terminal, cut: aterm_core::terminal::HolderCut) {
    if cut.to.is_some() || t.holder_fence().current_token().is_some() {
        t.note_holder_cut(cut);
        crate::metrics::note_holder_cut();
    }
}

/// Batch messages from the gather stage to the parse stage (unix pipeline).
/// `pub(crate)` for [`crate::manual_reset::ResetLane`], which holds a clone of
/// the sender so a manual reset rides the same channel as the batches.
#[cfg(unix)]
pub(crate) enum GatherMsg {
    /// The deferred start gate released — the parse stage may post `Ready`.
    Started,
    /// Every byte the master held when this reader started, which a park left
    /// unread ([`UpdatePark`]), went out in the batches before this one: once
    /// they are ingested, the parse stage ends the park it was attached after.
    CaughtUp,
    /// One gathered batch (buffer, filled length, the foreground change it
    /// carries, the group whose bytes END it — `edge.to` with an edge — and,
    /// with an edge, `edge.to`'s image as the gather read it at the edge,
    /// `crate::holder_image`). The parse stage returns the buffer on the free
    /// channel after processing, offers the image before the hold of the slice
    /// at the edge's offset, and runs the foreground handback in that hold.
    Data(
        Vec<u8>,
        usize,
        Option<crate::foreground_handback::FgEdge>,
        i32,
        Option<Arc<crate::holder_image::HolderImage>>,
    ),
    /// A foreground edge with no byte in flight (card engine §3.3): an `exec`
    /// the process watch reported before the next byte, or one heard while the
    /// batch before it already carried its edge. The edge, the holder after
    /// it and the image the gather read at the event; the parse stage runs it
    /// as an empty batch, so its publication and its cut precede every byte
    /// read after it.
    Edge(
        crate::foreground_handback::FgEdge,
        i32,
        Option<Arc<crate::holder_image::HolderImage>>,
    ),
    /// The master EOF'd on its own (shell exited).
    Eof,
    /// The session's shell exited (its exit watch fired before the master's
    /// EOF), and what the master held then has been sent: post `Wake::Exit`
    /// and go on consuming, since a job the shell left behind may still hold
    /// the terminal. The gather ends later with `Wake`.
    Exited,
    /// Session teardown (wake pipe / stop flag) — exit quietly.
    Wake,
    /// A manual reset (`crate::manual_reset`): the `reset` verb or Edit ▸ Reset
    /// Terminal, sent through the session's [`crate::manual_reset::ResetLane`].
    /// The parse stage runs it between two batches, as it runs a foreground
    /// handback at a cut.
    Reset(crate::manual_reset::ManualReset),
    /// A host path changed the engine with no output to follow (a resize, a
    /// config or cell-size change): bring the session's crash-restore journal up
    /// to date now (`crash_restore::SessionJournal::on_nudge`). Sent through the
    /// session's [`crate::manual_reset::ResetLane`] (`nudge`); a reader with no
    /// journal ignores it.
    Journal,
}

/// The gather stage of the split PTY pipeline: a thread that only drains the
/// master into recycled 64 KiB batches, so the kernel's small tty output queue
/// is re-armed while the parse stage runs concurrently; flood throughput is
/// min(drain, parse). Owns the master poll loop, the wake pipe and the deferred
/// start gate, and always ends with an `Eof`/`Wake` message or channel close,
/// so the parse stage needs no timeout. When the parse stage holds every
/// buffer, the gather blocks on the free channel and the child is throttled.
#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn spawn_pty_gather(
    master: i32,
    wake_rd: i32,
    // The shell's exit watch: once it fires, the gather passes on what the
    // master holds (at most [`EXIT_DRAIN_MAX`]) and ends the session as at EOF.
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
    // This session's byte sink, so the `O_NONBLOCK` flip below can be declared
    // to it. `O_NONBLOCK` belongs to the open file description, which the sink
    // cannot observe; knowing it, the sink's UI-thread egress writes a whole
    // keystroke frame in one `write(2)` instead of one byte per `poll(2)`.
    sink: Arc<SinkWriter>,
    // The foreground probe (`tcgetpgrp`; a scripted fake under the Tier-1
    // bind), sampled at every dry-gap park and batch end so each batch is cut
    // where the foreground group changed (`crate::foreground_handback`).
    fg_probe: fn(i32) -> i32,
    // `Session::fg_holder`: the holder the previous reader last saw (the seed)
    // and where this one leaves its own.
    fg_holder: Arc<std::sync::atomic::AtomicI32>,
    // `SessionFactory::edge_image`: the new holder's image, read at each edge
    // the cut finds (card engine §3.4).
    edge_image: crate::holder_image::ReadImage,
    // The session's process watch (card engine §3.3): its descriptor joins
    // every poll, and an `exec` it hears is cut here (`crate::exec_edges`).
    proc_watch: Option<aterm_uds::procwatch::ProcWatch>,
    // The manual reset's lane generation (`crate::manual_reset`), held for the
    // thread's life: its drop removes the lane's clone of `filled_tx` on every
    // exit path, so the channel ends with this thread and the parse stage's
    // `recv()` returns.
    lane_guard: crate::manual_reset::LaneGuard,
    // The session's crash-restore journal: after every read, before the batch
    // is sent, the running count of bytes read is stored in its region, so a
    // restore counts what was read and never processed.
    journal: Option<Arc<crate::crash_restore::SessionJournal>>,
) -> Result<(), String> {
    // macOS: default QoS parks this thread on E-cores, whose wakeup latency
    // dwarfs the PTY producer/consumer cadence.
    crate::qos::spawn_lane_thread(
        format!("aterm-pty-gather-{id}"),
        crate::qos::Role::Responsive,
        move || {
            let _lane_guard = lane_guard;
            // Deferred start (overlap park): no master read before release. On a
            // stop-abort the parse stage still gets its terminal message.
            if let Some(gate) = &start_gate
                && !gate.wait_until_released(&stop)
            {
                let _ = filled_tx.send(GatherMsg::Wake);
                if wake_rd >= 0 {
                    aterm_pty::close_fd(wake_rd);
                }
                return;
            }
            // The foreground cut: the holder starts as the one the previous
            // reader last saw (`Session::fg_holder`, carried across a handoff),
            // so a job that died while no reader ran is still an edge at the
            // first sample. With nothing carried, it is whoever holds the
            // terminal now. Taken after the start gate releases and before
            // `Started`, so a reader reported ready has its holder.
            let carried = fg_holder.load(Ordering::Acquire);
            let seed = if carried > 0 {
                carried
            } else {
                fg_probe(master)
            };
            let mut cutter = crate::foreground_handback::FgCutter::new(seed);
            // THE KERNEL EDGES (card engine §3.3): the watch joins every poll
            // below, so a batch with no process event costs nothing more.
            let mut exec =
                proc_watch.map(|watch| crate::exec_edges::ExecEdges::new(watch, edge_image, seed));
            let proc_rd = exec.as_ref().map_or(-1, crate::exec_edges::ExecEdges::fd);
            // The edges heard with no byte in flight, each sent as a byte-free
            // batch (`GatherMsg::Edge`): one heard before the next byte, then
            // any heard while a batch already carried its edge, in order.
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
                            return false; // parse stage gone
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
            // CAUGHT UP: what the program wrote while no reader ran (an update's
            // park, round eight's lead U12) waits in the kernel queue, and until
            // it is parsed the parser speaks for the program as it was before.
            // Its depth is read now, before any read; the queue is FIFO, so once
            // that many bytes have gone out every one of them has, and
            // `CaughtUp` follows the batch that carried the last. A platform
            // that reports no depth owes nothing.
            let mut owed = aterm_pty::output_queue_len(master).unwrap_or(0);
            if owed == 0 {
                let _ = filled_tx.send(GatherMsg::CaughtUp);
            }
            // Non-blocking direct-read drain: with the master `O_NONBLOCK` the
            // top-up spins on `read(2)` without a `poll(0)` per chunk. Flipping
            // the shared description is safe because every writer goes through
            // the sink's pollout-retry emulation. If fcntl fails the master stays
            // blocking and the poll-guarded `drain_more` is used.
            let nonblock = aterm_pty::set_nonblocking(master, true).is_ok();
            // Declare the fcntl's actual result to the sink: claiming non-blocking
            // on a blocking description could park the event loop in `write(2)`,
            // while `false` only costs syscalls.
            sink.note_master_nonblocking(nonblock);
            let mut buf = first_buf;
            let mut exit_rd = shell_exit.as_ref().map_or(-1, ShellExit::as_raw_fd);
            // Once the shell has exited, the output the master may still pass on
            // before the exit is reported (`ReadOutcome::Exited`).
            let mut after_exit: Option<usize> = None;
            // The exit was reported (`GatherMsg::Exited`): the master's EOF is
            // then the end of a job's output, not a second exit.
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
                    // The shell is gone: what the master holds now, never waiting
                    // for more. Once it is empty (or the cap is spent) the exit is
                    // reported, and reading goes on without the watch.
                    Some(left) => {
                        let room = left.min(buf.len());
                        match aterm_pty::drain_more(master, &mut buf[..room], 0) {
                            0 => {
                                after_exit = None;
                                exit_rd = -1;
                                exit_reported = true;
                                if let Some(exit) = &shell_exit {
                                    exit.await_retired(SHELL_RETIRE_WAIT);
                                }
                                if filled_tx.send(GatherMsg::Exited).is_err() {
                                    break; // parse stage gone
                                }
                                continue;
                            }
                            n => aterm_pty::ReadOutcome::Data(n),
                        }
                    }
                };
                match outcome {
                    // The session ends with its shell, not the slave's last close
                    // (docs/AUDIT-pty-render-2026-10-09.md P3): a job the shell
                    // left behind can hold the slave for hours, and Linux does not
                    // hang a PTY up at its session leader's exit. What the master
                    // holds goes on, then `Exited` reports the exit; closing the
                    // tab closes the master, which hangs up whatever holds the slave.
                    aterm_pty::ReadOutcome::Exited => {
                        after_exit = Some(EXIT_DRAIN_MAX);
                    }
                    // An `exec` heard before the next byte: its edge, if it is
                    // one, goes byte-free, ahead of every byte read after it.
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
                        if !exit_reported && let Some(exit) = &shell_exit {
                            exit.await_retired(SHELL_RETIRE_WAIT);
                        }
                        let _ = filled_tx.send(if exit_reported {
                            GatherMsg::Wake
                        } else {
                            GatherMsg::Eof
                        });
                        break;
                    }
                    aterm_pty::ReadOutcome::Data(n) => {
                        // A reader test's process event, after this batch's
                        // first bytes (`holder_image::hold_hook::on_next_read`).
                        #[cfg(test)]
                        crate::holder_image::hold_hook::after_read(id);
                        // Top up past the ~1 KiB kernel chunk, hand the batch over,
                        // then take a recycled buffer: blocking on the free channel
                        // is the backpressure.
                        cutter.begin();
                        let filled = if nonblock {
                            aterm_pty::drain_more_nonblocking(
                                master,
                                &mut buf,
                                n,
                                wake_rd,
                                proc_rd,
                                Some(parse_in_flight.as_ref()),
                                // The same interactive-input-pending hint the
                                // parse stage slices its lock holds on (P05).
                                crate::input_hint::pending,
                                // The pre-park foreground sample: a change seen
                                // here delivers the batch NOW, cut at the last
                                // sample that still showed the old group. A
                                // park the watch ended hears it at the same
                                // offset: an `exec` edge delivers likewise.
                                &mut |f, heard| match exec.as_mut() {
                                    Some(x) if heard => {
                                        x.heard(&mut cutter, || fg_probe(master), f)
                                    }
                                    _ => cutter.sample(fg_probe(master), f),
                                },
                            )
                        } else {
                            // The blocking fallback never parks: batch end only,
                            // but its per-chunk polls hear the watch, at the
                            // offset each was made at.
                            aterm_pty::drain_more_watched(master, &mut buf, n, proc_rd, &mut |f| {
                                exec.as_mut()
                                    .is_some_and(|x| x.heard(&mut cutter, || fg_probe(master), f))
                            })
                        };
                        let mut edge = cutter.finish(|| fg_probe(master), filled);
                        // THE HOLDER'S IMAGE AT THE EDGE (card engine §3.4): read
                        // here, at the sample that found the new group, not when
                        // the parse stage reaches the cut, so a lagging parse
                        // stage never pairs a later image with this cut. A
                        // kernel edge's was read at its event. A sampled one
                        // first watches its new leader and hears what is
                        // queued (`ExecEdges::absorb`): an `exec` of that
                        // leader may have written anywhere after the cut, so
                        // its run begins at the batch's end.
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
                        // Timestamp the completed kernel drain before publishing the
                        // batch. The parse-side channel receive and reader join form
                        // the happens-before path to the updater's Acquire check.
                        let output_activity_now =
                            u64::try_from(lat_epoch.elapsed().as_nanos()).unwrap_or(u64::MAX);
                        latest_output_activity_ns
                            .store(output_activity_now.max(1), Ordering::Release);
                        // Counted before the send: the bridge never sees "idle"
                        // while a batch is queued unparsed, and the reader's
                        // fetch_sub never precedes this add (usize underflow would
                        // seal the cutoff open).
                        parse_in_flight.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        // The journal's read count, after the read and before the
                        // send: a crash between the two counts these bytes lost
                        // rather than restore a screen that silently lacks them.
                        if let Some(journal) = &journal {
                            journal.gathered(filled);
                        }
                        if filled_tx
                            .send(GatherMsg::Data(buf, filled, edge, holder, image))
                            .is_err()
                        {
                            break; // parse stage gone (session torn down)
                        }
                        if owed > 0 {
                            owed = owed.saturating_sub(filled);
                            if owed == 0 {
                                let _ = filled_tx.send(GatherMsg::CaughtUp);
                            }
                        }
                        // The `exec`s heard while this batch carried its edge.
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
                            Err(_) => break, // parse stage gone
                        };
                    }
                }
            }
            // The gather owns the wake pipe's read end on unix: close it on exit.
            aterm_pty::close_fd(wake_rd);
        },
    )
    .map(|_| ())
    .map_err(|error| format!("spawn PTY gather: {error}"))
}

/// The foreground handback's share of a reader's wiring
/// (`crate::foreground_handback`). Unix only: ConPTY has no foreground group.
#[cfg(unix)]
struct ForegroundWiring {
    /// [`SessionFactory::fg_probe`], sampled by the gather.
    probe: fn(i32) -> i32,
    /// [`SessionFactory::fg_gone`], asked by the parse stage at an edge.
    gone: crate::foreground_handback::FgGone,
    /// `Session::fg_holder`: the gather's seed, and where it leaves its holder.
    holder: Arc<std::sync::atomic::AtomicI32>,
    /// The shell's pid, which is its group's: a slice another group holds is a
    /// job's output (`Terminal::set_foreground_job`).
    shell: i32,
    /// [`SessionFactory::edge_image`]: read by the gather at an edge and by the
    /// parse stage's byte arm.
    image: crate::holder_image::ReadImage,
    /// [`SessionFactory::edge_inbox`].
    inbox: crate::holder_image::EdgeInbox,
    /// [`SessionFactory::byte_arm`].
    byte_arm: bool,
    /// [`SessionFactory::proc_watch`]'s watch on this shell, moved into the
    /// gather; with one, the byte arm runs its initial read alone.
    proc_watch: Option<aterm_uds::procwatch::ProcWatch>,
}

/// The owned wiring [`spawn_pty_reader`] moves into this session's reader thread.
/// The caller makes every `Arc`/`Sender` clone, so the channels stay open for
/// the thread's whole life.
struct PtyReaderWiring {
    master: i32,
    id: u64,
    window: WindowId,
    term: Arc<Mutex<Terminal>>,
    /// Where this reader's `Wake`s go (the event loop, in every shipping attach).
    post: WakePost,
    /// Query replies go to the reply-writer thread, never written inline
    /// ([`REPLY_QUEUE_CAP`]).
    reply_tx: ReplyLane,
    /// Compression-worker signal (`None` when the worker could not spawn: offload
    /// inactive, the reader drains inline). The reader `try_send`s a token after a
    /// burst once its lazy backlog crosses `COMPRESS_SIGNAL_AT`.
    compress_tx: Option<std::sync::mpsc::SyncSender<()>>,
    /// The session's cast recorder, which the reader records each burst into after
    /// the holds that processed it ([`crate::cast::CastRecorder::record_burst`]).
    /// Its lock is never taken under `term_lock`, and every hold of it is bounded.
    cast: Arc<std::sync::Mutex<crate::cast::CastRecorder>>,
    /// The cast recorder's epoch ([`crate::cast::CastRecorder::epoch`]), copied
    /// once per attach: the reader puts each burst's arrival reading on it (read
    /// outside every `term_lock` hold) without the recorder's lock.
    cast_epoch: Instant,
    /// The cast recorder's watermark ([`crate::cast::CastWatermark`]): each hold
    /// that processes a burst counts itself there, so `cast drift` can tell
    /// whether the recording holds the screen it read.
    cast_watermark: Arc<crate::cast::CastWatermark>,
    /// `None` when temporal recording is disabled (the default): the reader then
    /// skips the `RawIn`/`Reply` spine taps entirely (no writer thread exists).
    /// BOUNDED ([`TEMPORAL_QUEUE_CAP`]) like `reply_tx` — the reader
    /// `try_send`s and drops on a full queue rather than blocking or growing unbounded.
    temporal_tx: Option<crate::temporal::TemporalTx>,
    /// The spine's resize watermark for this reader, attached to the session's
    /// ([`crate::temporal::SpineMark::attach`]): it starts where the previous
    /// reader left the spine. Unused (and unshared) when `temporal_tx` is `None`.
    spine_mark: crate::temporal::SpineMark,
    byte_fanout: Arc<crate::cast::ByteFanout>,
    /// The session's watchers, rung once per burst after the engine took it
    /// and the fan-out holds it ([`publish_burst`]).
    watchers: Arc<crate::session_watch::Watchers>,
    /// This session's byte sink, carried for two uses: the gather holds it to
    /// declare its `O_NONBLOCK` flip (see [`spawn_pty_gather`]'s `sink`
    /// parameter), and the parse stage takes a `Weak` of it, upgraded around
    /// each foreground-edge probe so the master number it names stays open
    /// for that probe. The reader itself never writes through it; replies go
    /// via `reply_tx`. Unix only: the Windows reader has no gather and no
    /// foreground edge.
    #[cfg(unix)]
    sink: Arc<SinkWriter>,
    lat_epoch: Instant,
    /// The event loop's turn state (`SessionFactory::loop_idle`): read at each
    /// wake edge so an armed latch is re-posted only when its wake was stranded
    /// while the loop slept (see [`gated_output_wake`]).
    loop_idle: Arc<LoopIdle>,
    last_output_ns: Arc<AtomicU64>,
    /// Most recent consumed PTY burst (ns on `lat_epoch`; 0 = none). Overwritten
    /// on every read and never cleared by a present; updater quiet admission ages
    /// this stamp independently of the first-edge presentation metric above.
    latest_output_activity_ns: Arc<AtomicU64>,
    /// Stamped once when the PTY closes on its own, just before `Wake::Exit`
    /// is posted (shared with the owning `Session::pty_closed_at`).
    pty_closed_at: Arc<std::sync::OnceLock<Instant>>,
    /// `Wake::Output` coalescing latch shared with the owning `Session` (arm
    /// timestamp ns; 0 = clear): at most one event per main-thread handler pass,
    /// never losing the final burst. Self-expiring, see [`gated_output_wake`].
    output_wake_pending: Arc<AtomicU64>,
    /// UI-waiter register shared with the owning `Session` (`SessionCtx::ui_waiting`):
    /// non-zero while a UI-thread acquisition is parked on this terminal's mutex.
    /// Read between slices so the boundary becomes a handoff.
    ui_waiting: Arc<std::sync::atomic::AtomicU32>,
    /// Read end of this session's wake pipe: `Session::drop` writes the write end
    /// to break the reader out of a `read` an orphaned child keeps from EOF. `-1`
    /// when no pipe could be made (the fallback poll). Owned by the thread that
    /// reads the master (the gather on unix), which closes it on exit; when
    /// [`spawn_pty_reader`] returns `Err` the caller closes it.
    wake_rd: i32,
    /// Fd-free stop flag shared with the owning `Session`, used only when
    /// `wake_rd < 0` (pipe creation failed under fd exhaustion): the timed
    /// fallback poll checks it each tick, so the session is still reclaimed.
    stop: Arc<AtomicBool>,
    /// The shell's exit watch ([`shell_exit_watch`]), moved into the gather and
    /// closed with it. `None`: the session ends at the master's EOF alone.
    #[cfg(unix)]
    shell_exit: Option<ShellExit>,
    /// Overlap child preparation gate. `Some` means every reader/helper resource
    /// exists, but this thread must consume zero PTY bytes until parent Commit.
    start_gate: Option<DeferredReaderGate>,
    /// Preallocated before the thread is spawned/proof is emitted.
    read_buf: Vec<u8>,
    /// The foreground handback's probes and carried holder.
    #[cfg(unix)]
    fg: ForegroundWiring,
    /// The session's event timeline — the reader records `modes-restored` on
    /// it after a foreground handback, with no other lock held.
    timeline: Arc<std::sync::Mutex<crate::session_timeline::SessionTimeline>>,
    /// [`SessionFactory::card_probe`].
    card_probe: bool,
    /// The session's person count and launch witness: in the hold whose slice
    /// started a command, the parse stage records where the count stood
    /// (`crate::launch_witness`).
    person: Arc<crate::human_input::HumanInputStamp>,
    launch: Arc<crate::launch_witness::LaunchWitness>,
    /// The session's manual-reset lane (`crate::manual_reset`): the unix
    /// reader opens it with a clone of its gather→parse sender and closes it
    /// when either stage ends. Windows has no parse channel and leaves it
    /// closed, so a reset there runs directly under the term lock.
    reset_lane: Arc<crate::manual_reset::ResetLane>,
    /// What the warm spare's gate reads of this session (`SessionCtx::witness`):
    /// the parse stage records a signed parked mark and start record after each
    /// chunk, and closes a spare's FIFO at its first prompt.
    witness: Arc<crate::spare::Witness>,
    /// The session's crash-restore journal (`crate::crash_restore`), when it has
    /// one: the gather stores its read count, the parse stage records every
    /// hold's bytes and resizes and writes the keyframes its policy calls for.
    #[cfg(unix)]
    journal: Option<Arc<crate::crash_restore::SessionJournal>>,
    /// The session's update park and the mark it stood at when this reader was
    /// attached ([`UpdatePark`]): the parse stage ends that park once it has
    /// ingested what the gather owed it ([`GatherMsg::CaughtUp`]).
    #[cfg(unix)]
    park: (Arc<UpdatePark>, u64),
}

/// PTY reader thread for one session: read, feed this engine, and wake the UI
/// with this session's id so `user_event` routes output/EOF to the right tab.
/// The join handle is kept on `Session.reader_join` as the overlap handoff's
/// park acknowledgement ([`park_reader`]); other callers may drop it. On `Err`
/// no thread holds `wake_rd`: the caller still owns it and closes it.
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
    // The parse stage's liveness probe (the orphan half of the foreground
    // handback). ConPTY has no foreground group, so off unix nothing is gone.
    #[cfg(unix)]
    let (fg_gone, fg_shell) = (fg.gone, fg.shell);
    #[cfg(not(unix))]
    let (fg_gone, fg_shell): (crate::foreground_handback::FgGone, i32) = (|_, _, _| false, 0);
    // The holder image's half of the edge (card engine §3.4-§3.6): the reader
    // the byte arm re-reads with, the App's edged inbox, and whether the arm
    // runs. ConPTY has no foreground group: no edge, no arm.
    // With a process watch (card engine §3.3) the arm makes its initial read
    // alone: the kernel's `exec` edges order every later one.
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
    // The unix pipeline's two stages: this thread becomes the parse stage, fed
    // recycled 64 KiB batches over a bounded channel by the gather. The gather
    // is spawned last: it owns the wake pipe's read end and closes it on every
    // exit, so it may run only once nothing after it can fail. A gather-spawn
    // failure drops its unrun closure, and with it the batch sender and the lane
    // guard: the parse stage's `recv()` returns `Err` and it exits quietly.
    #[cfg(unix)]
    let (filled_rx, free_tx, parse_in_flight, lane_generation, probe_sink, start_gather) = {
        let (filled_tx, filled_rx) =
            std::sync::mpsc::sync_channel::<GatherMsg>(GATHER_RING_BUFFERS + 4);
        // The manual reset's way onto this channel, opened before the gather
        // exists so its guard can own the close.
        let lane_generation = reset_lane.open(filled_tx.clone());
        let lane_guard = crate::manual_reset::LaneGuard {
            lane: reset_lane.clone(),
            generation: lane_generation,
        };
        let (free_tx, free_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(GATHER_RING_BUFFERS);
        for _ in 1..GATHER_RING_BUFFERS {
            let _ = free_tx.try_send(vec![0u8; 65_536]);
        }
        // Batches handed to the parse stage and not yet fully ingested. The
        // gather's bridge reads 0 as "parser idle — deliver now, don't park"
        // (see drain_more_nonblocking); a heuristic, so Relaxed everywhere.
        let parse_in_flight = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        // The parse stage's edge probe names the master by number, valid only
        // while some sink clone lives. A `Weak`, upgraded around each probe,
        // so the parse stage never delays the close.
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
    // ConPTY has no foreground group: no edge, so no probe reaches the master.
    #[cfg(not(unix))]
    let probe_sink = std::sync::Weak::<SinkWriter>::new();
    let parse = crate::qos::spawn_lane_thread(
        format!("aterm-pty-reader-{id}"),
        crate::qos::Role::Responsive,
        move || {
            // macOS: default QoS parks this thread on E-cores, whose wakeup
            // latency dwarfs the PTY cadence and costs drain throughput.
            // Windows keeps the single-thread read+parse loop (ConPTY has no
            // ~1 KiB read cap, so the gather split buys nothing there); the gate
            // and buffer live with whichever thread reads the master.
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
            // PTY read buffer is a fixed 64 KiB, allocated before spawn/proof.
            // Temporal resize watermark: the engine resize ordinal up to which the
            // spine holds every resize (`crate::temporal::SpineMark`), attached to
            // the session's watermark so it starts where the keyframe or previous
            // reader left off. Each hold that processes bytes first records every
            // resize journalled since (net-zero flaps included; past the journal's
            // depth they are counted as `lost_resizes=`). A resize waits for the
            // program's next output, shown as `pending_resizes=` meanwhile.
            // It also carries a break (input dropped on a full queue) until a hold
            // queues a keyframe of the live engine. Records under `term_lock` with
            // `process`; only this reader uses it.
            let mut spine_mark = spine_mark;
            // Readiness (Windows): this thread is live and about to read, so flip
            // the registry handle `Spawning -> Alive` before the first blocking
            // read, so a shell that emits no output still confirms promptly. The
            // main thread registers `Spawning` before draining this `Wake`, so the
            // transition never precedes the handle. Headless, `send_event` fails
            // and the session stays `Spawning` but addressable. Unix posts Ready
            // from the parse loop's `Started` arm.
            #[cfg(windows)]
            let _ = post(Wake::Ready {
                session: id,
                window,
            });
            // One gathered batch through the engine and every tap, ordered to keep
            // the screen closest to the bytes: latency stamp, sliced `process()`
            // under the term lock (plus temporal spine), compress signal, query
            // replies, coalesced wake, then the cast/byte taps, which no pixel
            // depends on. Shared by the unix parse loop and the Windows inline loop.
            //
            // `edge` (unix gather only): the foreground group changed at `edge.at`.
            // A slice ends there and, under that slice's lock, runs the foreground
            // handback (`crate::foreground_handback`) before any byte the new holder
            // wrote, when the modes in force were armed by a group that is gone
            // (`fg_owners`). `holder`: the group whose bytes end the batch
            // (`edge.to`, or `0` unknown and always on Windows).
            //
            // `pending_reset` (unix; `crate::manual_reset`): a manual reset handed
            // to the next call, made with an empty batch, so the reset lands
            // between two PTY batches and is recorded like a handback. A `Cell`
            // because the loop sets it while `ingest` holds a shared borrow.
            let pending_reset: std::cell::Cell<Option<crate::manual_reset::ManualReset>> =
                std::cell::Cell::new(None);
            let mut fg_owners = crate::foreground_handback::FgOwners::default();
            // The platform reads a slice can ask for, done before its hold
            // (`crate::host_io_prefetch`), and what the session admits of them.
            let mut prefetch = crate::host_io_prefetch::Prefetch::default();
            // This reader's `Wake::Output` posts: whether its next one is a
            // streaming session's ([`PostCadence`]).
            let mut cadence = PostCadence::default();
            // THE OUTPUT WAKE, one post path for the batch's end and a holder
            // image's publication: coalesce wakes, posting `Wake::Output` only
            // on the latch's clear-to-armed edge ([`gated_output_wake`]). The
            // arm timestamp is read here, not the burst's arrival stamp, so the
            // latch's expiry measures only how long a posted wake has gone
            // unhandled.
            //
            // The read after the post is the latch's sent stamp: the watchdog
            // presumes a wake lost only once a user-event drain began after it
            // ([`judge_output_wake`]).
            //
            // An armed latch is re-posted only when its wake was stranded while
            // the loop slept, never one queued behind a long frame.
            //
            // A post in a burst that has run a frame says the session is
            // streaming, and its pass takes a hold ([`PostCadence`],
            // [`OUTPUT_CADENCE_NS`]): a line-at-a-time writer then costs the
            // loop one turn per cadence, not one per read.
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
            // THE BYTE-DRIVEN ARM (card engine §3.6, `crate::holder_image`),
            // when this reader runs it: seeded from the timeline, so a reader
            // re-attached mid-launch still re-reads a published Launcher.
            let mut arm = byte_arm.then(|| {
                let executing = {
                    let t = term_lock(&term);
                    t.is_require_shell_integration_nonce()
                        && t.shell_state() == aterm_core::terminal::ShellState::Executing
                };
                let arm = crate::holder_image::ByteArm::seeded(&timeline, executing);
                if exec_edges { arm.kernel_edges() } else { arm }
            });
            // The dev `card-seen` probe's budget per drawer token: how many
            // more batches may look for its card (`DEV_ROWS` builds only).
            let mut card_probe: Option<(u64, u16)> = None;
            // THE HOLDER IMAGE'S PUBLICATION (card engine §3.4 step 2), before
            // the hold of the slice it is offered at: the timeline (a leaf lock,
            // nothing else held), then, when it published, this session's id in
            // the App's edged inbox and the coalesced output wake, so the status
            // sweep names the program on its next turn (§3.5).
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
                // Stamp the leading edge of this output burst (a load and at most
                // one CAS) for output-to-present latency in `metrics` and the
                // `$ATERM_TRACE_LATENCY` log. `compare_exchange(0, …)` keeps the
                // first edge of a burst spanning several reads.
                //
                // Stamped before the process loop, so the term-lock wait and VT
                // parse, which grow when the terminal falls behind a flood, are
                // inside the measured interval.
                // A manual reset arrives as an empty batch and stamps neither an
                // output edge nor an echo round trip.
                // The same reading is the burst's time on the cast timeline,
                // taken outside every term-lock hold. `read_at`: when the batch's
                // first read returned, where it was gathered before being fed
                // (the Windows reader's held gather, `gather_held_output`).
                let arrived = (!buf.is_empty()).then(|| read_at.unwrap_or_else(Instant::now));
                if let Some(at) = arrived {
                    stamp_output_arrival(
                        &last_output_ns,
                        (at.saturating_duration_since(lat_epoch).as_nanos() as u64).max(1),
                    );
                    // Echo round trip: the first bytes back from this session's
                    // child after a keystroke went out, stamped before the lock wait
                    // and parse so the number is the child's round trip, not aterm's
                    // ingest backlog (see `echo_rtt`). One relaxed load per burst when
                    // no keystroke is outstanding.
                    crate::echo_rtt::note_output_burst(id);
                }
                // A foreground edge in this batch (a program started or ended)
                // ends the output burst: what follows it, the shell's marks and
                // prompt or a new program's output, is not the old program's
                // stream, so this batch's post goes out at once rather than at a
                // hold's release ([`PostCadence::foreground_edge`]).
                if edge.is_some() {
                    cadence.foreground_edge(&output_wake_pending);
                }
                // THRU-5: deferred-compression backlog after this burst (set under the
                // process lock below), read afterward to decide whether to wake the worker.
                let mut backlog = 0usize;
                // The foreground handback this batch ran, if any (see the slice loop).
                let mut handback: Option<aterm_core::terminal::ForegroundHandback> = None;
                // A command start this batch parsed whose screen is still to be
                // read, after the lock (`crate::launch_witness`).
                let mut launch_unread: Option<crate::launch_witness::Unread> = None;
                // A dead app's alternate screen the engine asks this session's
                // console host to leave (OpenConsole only), taken in the hold
                // that parsed the prompt and acted on after the last one
                // (`crate::alt_screen_resync`).
                let mut resync_due = false;
                // The manual reset this call ran, if any, with its bytes.
                let mut manual_run: Option<(
                    crate::manual_reset::ManualReset,
                    crate::manual_reset::ResetReport,
                    Vec<u8>,
                )> = None;
                // Engine-ordered cast: where this burst sits in the engine's order,
                // noted inside every hold that processes its bytes and cut where a
                // resize took the lock between slices (`crate::cast`). `emitted` is
                // the offset in the burst as the cast tap receives it: the manual
                // reset's bytes, then the slices with any handback's bytes at the
                // cut. Cost inside the lock: two loads, a compare and a relaxed
                // increment (the hold's count on the recorder's watermark) per
                // hold, no clock read. A buffer is allocated only for a burst a
                // resize cut.
                let mut cuts = crate::cast::CastCuts::default();
                let mut emitted = 0usize;
                // The batch's one heap copy while the temporal spine records,
                // made before the first hold: each slice's `RawIn` is a range of
                // it, so a hold queues a refcount and allocates and copies
                // nothing, and the cast and byte taps below carry it unless a
                // splice changed the bytes. Not recording, the taps copy the
                // batch after the wake instead. The engine processes the copy,
                // so the spine holds exactly the bytes it saw.
                let spine_copy: Option<std::sync::Arc<[u8]>> =
                    temporal_tx.as_ref().map(|_| std::sync::Arc::from(buf));
                // A crash-restore keyframe a hold captured (`crate::crash_restore`):
                // encoded and copied into its slot once that hold's lock is released,
                // before the next hold appends to the tail.
                #[cfg(unix)]
                let mut journal_keyframe: Option<crate::crash_restore::Captured> = None;
                let (response, reports) = {
                    let bytes: &[u8] = spine_copy.as_deref().unwrap_or(buf);
                    // The manual reset, before any byte of `buf` and under one
                    // term-lock hold: the engine resets, the synthesized bytes go on
                    // the spine (geometry first), and what the reset cleared is
                    // credited to no holder.
                    if let Some(req) = pending_reset.take() {
                        let mut t = term_lock(&term);
                        cuts.observe(emitted, &t, &cast_watermark);
                        let spine_pre = temporal_tx
                            .as_ref()
                            .map(|_| crate::temporal::SpinePre::take(&t));
                        // The crash-restore journal's reading before the reset, in
                        // the same hold: the reset's bytes go on its tail where they
                        // go on the spine, and a keyframe it makes due is captured at
                        // the hold's end and written once the lock is released.
                        #[cfg(unix)]
                        let mut journal_pre = journal.as_ref().map(|j| j.pre(&t));
                        let (report, reset_bytes) = crate::manual_reset::apply(&mut t);
                        emitted += reset_bytes.len();
                        // Its assertions are taken here, as the automatic
                        // handback's are, so none is credited to the next
                        // slice's holder (a reset only disarms: none).
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
                    // The reset's keyframe, if it made one due: written with the lock
                    // released and before the first slice appends anything.
                    #[cfg(unix)]
                    if let (Some(j), Some(captured)) = (&journal, journal_keyframe.take()) {
                        j.write(captured);
                    }
                    // Slice the burst so the term lock is released between chunks,
                    // choosing the width per hold:
                    //   * a keystroke waiting to echo (`input_hint::pending`): 8 KiB
                    //     slices, so the press path's lock hold and the echo present
                    //     queue behind at most about one chunk;
                    //   * no key in flight (a pure flood): the rest of the burst in
                    //     one hold, fewer round trips against the renderer's per-frame
                    //     hold; the next hold is fine-sliced again;
                    //   * either way, a slice also ends where the memory budget of the
                    //     platform reads done before its hold runs out
                    //     (`crate::host_io_prefetch`).
                    // The VT parser is a streaming state machine, so slicing is
                    // byte-identical to one `process()`, and concatenated per-chunk
                    // replies equal the single-call result. Small bursts are one chunk.
                    let mut acc: Option<Vec<u8>> = None;
                    let mut reports: Option<Vec<u8>> = None;
                    let mut off = 0;
                    // The handback offset, clamped to the batch; consumed once.
                    let mut handback_at = edge.map(|e| e.at.min(bytes.len()));
                    // Where the new holder's run begins when that is past the
                    // cut (an edge a process event found, card engine §3.3):
                    // the bytes between are no holder's, and the new drawer's
                    // mark is laid there, in a hold of its own.
                    let mut lay_at = edge
                        .map(|e| e.lay.min(bytes.len()))
                        .filter(|lay| Some(*lay) > handback_at);
                    // The drawer the edge's image is, by its token: the only
                    // holder the engine fence keeps a mark for.
                    let drawer_to = image
                        .as_ref()
                        .filter(|i| i.drawer.is_some())
                        .and_then(|i| i.token);
                    // Whose bytes the next slice holds: the old holder's before
                    // the cut, the batch-end holder's from it on.
                    let mut slice_holder = edge.map_or(holder, |e| e.from);
                    // The byte arm's batch start (card engine §3.6): where its
                    // slices end after an OSC 133 C, and the one ordinary
                    // observation this nonempty batch may owe.
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
                        // A slice never crosses the handback offset, nor the
                        // new holder's lay.
                        let limit = match handback_at {
                            Some(at) if at > off => at,
                            _ => bytes.len(),
                        };
                        let limit = match lay_at {
                            Some(lay) if lay > off => limit.min(lay),
                            _ => limit,
                        };
                        // Nor, while the byte arm runs, the end of an OSC 133
                        // C: the re-read the C owes runs before the next byte.
                        let limit = arm
                            .as_ref()
                            .and_then(|a| a.next_cut(off))
                            .filter(|_| {
                                // The arm's no-cut mutant (a control, tests only).
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
                        // The pasteboard and Kitty medium reads this slice asks for,
                        // done before its hold: a read that blocks holds this reader,
                        // never the lock every window's redraw takes. The slice ends
                        // where their memory budget does.
                        let fetched = prefetch.reads(&term, &bytes[off..end]);
                        let end = off + fetched.covered();
                        // Liveness is decided before the term lock (a `kill(pgid, 0)`,
                        // a member listing for a reaped leader, a `kill(-pgid, 0)` for
                        // any other owner): under it, it would stall render and input
                        // at every command completion. The master is named only while
                        // a sink clone keeps it open (`held` across the probe): after a
                        // tab close the gather may drop the last clone while its queued
                        // batches are still ingested, and a closed number can already
                        // name another descriptor. `-1` sends the Linux member walk to
                        // its `/proc` fallback.
                        let verdicts = match (handback_at, edge) {
                            // An `exec` in place hands nothing back: no group
                            // lost the terminal.
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
                        // THE HOLDER IMAGE, published before this slice's hold
                        // (I1): at the cut, the image the gather read at the
                        // edge; elsewhere, while the byte arm runs, a re-read
                        // of an image that may have changed in place (an
                        // `exec` after a signed C, a Launcher's `exec`), an
                        // Exec edge at this slice's start — or, the arm's
                        // first read of a group, the holder as found: a Group
                        // edge, as its cut below is.
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
                        // THE ARM'S CUT (card engine §3.6): a re-read that
                        // names another image than the arm last noted for
                        // this group is an `exec` in place; the first read of
                        // a group is the holder as found. Either moves the
                        // engine fence in this slice's hold, before its bytes.
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
                        // Published here, or, under the I1 bind's control
                        // mutant only, after the hold.
                        let offered = offered.and_then(|(image, kind)| {
                            #[cfg(test)]
                            if crate::holder_image::hold_hook::publish_after_hold(id) {
                                return Some((image, kind));
                            }
                            publish_holder(image, kind, arm.as_mut(), &mut cadence);
                            None
                        });
                        // A signed C this slice parsed (the byte arm's mark).
                        let (c_signed, shell_executing);
                        {
                            let waited = term_busy(&term).then(Instant::now);
                            let mut t = term_lock(&term);
                            crate::metrics::note_parse_hold(parse_hold_wait(waited));
                            // The burst's ring comes after its tee
                            // (`publish_burst`), not at each slice's release.
                            t.holder_rings();
                            cuts.observe(emitted, &t, &cast_watermark);
                            // Temporal spine: the engine's resize ordinal and geometry
                            // before this chunk. External resizes run under term_lock
                            // between chunks and are journalled; `process()` never
                            // resizes this engine (DECCOLM is flag-only, XTWINOPS 8
                            // goes through the host). Read only when recording.
                            let spine_pre = temporal_tx
                                .as_ref()
                                .map(|_| crate::temporal::SpinePre::take(&t));
                            // The crash-restore journal's reading before this chunk:
                            // the handback's bytes and the chunk's ride its tail where
                            // they ride the spine, in this same hold.
                            #[cfg(unix)]
                            let mut journal_pre = journal.as_ref().map(|j| j.pre(&t));
                            // The foreground handback, exactly at the cut: after every
                            // byte the old holder wrote, before any the new holder wrote.
                            // The synthesized bytes go on the spine (geometry first)
                            // before this slice's RawIn, so replay reaches this state.
                            //
                            // Only orphaned modes are restored: when a group that armed
                            // an input-hijacking mode still in force is gone. A stopped
                            // job, a stopped or `bg`'d pipeline, a live gdb handing the
                            // terminal to its inferior, and a one-shot `tput smcup`/`civis`
                            // keep them. A gone old holder with nothing orphaned gets
                            // the torn-sequence CAN alone. The verdicts were taken
                            // before the lock.
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
                                // What the handback cleared belongs to nobody now,
                                // so the new holder re-arming it is the new
                                // holder's (fish's kitty push after the cut).
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
                            // THE HOLDER CUT (card engine §3.4 step 3), right
                            // after the handback in the same hold: the engine
                            // fence's outgoing mark closes (sealed when its
                            // group lives on, dropped at an `exec` or a death)
                            // and a drawer's opens, at its lay when that is
                            // past the cut. Every edge (sampled or kernel) and
                            // every arm observation that touches a drawer or
                            // the fence's current mark; no other.
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
                            // Whose bytes this slice holds, as the engine's
                            // OSC 7 prompt rule reads them: a group other than
                            // the shell's is a job's, one a key binding started
                            // included (`Terminal::set_foreground_job`). ConPTY
                            // names no holder: there the program probe alone
                            // speaks, asked by the engine.
                            t.set_foreground_job(slice_holder > 0 && slice_holder != fg_shell);
                            // The I1 bind's look from inside the hold, as its
                            // bytes are parsed: the timeline's token and the
                            // engine fence's.
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
                            // THE LAUNCH MARK, in this same hold: a slice that
                            // starts a command records the person count as it
                            // stands now, before any frame can show a byte of
                            // the slice (`crate::launch_witness`, "The proof").
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
                            // The warm spare's marks, read under this hold
                            // (`crate::spare::witness_after_process`).
                            #[cfg(unix)]
                            crate::spare::witness_after_process(&witness, &mut t, master, || {
                                probe_sink.upgrade().map(|sink| sink.driven_input())
                            });
                            prefetch.held(&t);
                            emitted += end - off;
                            // A mode this slice armed again belongs to its holder too
                            // (`take_evidence_asserted`), so a stale owner hides no death.
                            fg_owners.observe(
                                slice_holder,
                                t.program_evidence(),
                                t.take_evidence_asserted(),
                            );
                            // The reports the engine sends unasked (a resize's DEC
                            // 2048 report, a DSR 997 push) first, apart: they go
                            // the input's way (`ReplyLane::submit_report`).
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
                            // The deferred-compression backlog, read under the lock
                            // already held; the last chunk's value covers the burst.
                            backlog = t.lazy_backlog_len();
                            // Record this chunk (input plus every preceding external
                            // resize) under the term_lock, so spine order equals engine
                            // order. On a full queue the chunk's RawIn is dropped, never
                            // queued ahead of a resize it follows, and counted; the
                            // break it opens is closed by a keyframe of the live engine
                            // at the next hold that can queue one. The RawIn is this
                            // chunk's range of the batch's one copy (`spine_copy`).
                            if let (Some(tx), Some(pre), Some(copy)) =
                                (&temporal_tx, spine_pre, &spine_copy)
                                && end > off
                            {
                                let input =
                                    crate::temporal::InputSlice::new(copy.clone(), off..end);
                                spine_mark.record(tx, pre, &t, input);
                            }
                            // The chunk on the crash-restore journal's tail, after
                            // `process` (a chunk that panics the parser never reaches
                            // it), then the keyframe policy's decision at the hold's end.
                            #[cfg(unix)]
                            if let (Some(j), Some(pre)) = (&journal, journal_pre.as_mut()) {
                                j.record(&t, pre, &bytes[off..end], false);
                                journal_keyframe = j.decide(&t, pre);
                            }
                        }
                        // The keyframe the hold captured, with its lock released.
                        #[cfg(unix)]
                        if let (Some(j), Some(captured)) = (&journal, journal_keyframe.take()) {
                            j.write(captured);
                        }
                        // The control mutant's late publication (tests only).
                        if let Some((image, kind)) = offered {
                            publish_holder(image, kind, arm.as_mut(), &mut cadence);
                        }
                        // The accepted C owes an immediate boundary read and
                        // keeps its unresolved shell watched across batches.
                        // A signed completion/prompt ends that watch.
                        if let Some(a) = arm.as_mut() {
                            a.note_shell(c_signed, shell_executing);
                        }
                        drop(fetched);
                        off = end;
                        // Lock handoff: on macOS's unfair mutex a parked UI thread
                        // needs 10-30 µs to wake while this loop re-takes the lock in
                        // ~100 ns. Spin, bounded, while a UI acquisition is registered
                        // so neither intra-batch slices nor back-to-back batches from
                        // `filled_rx` starve the UI thread.
                        crate::yield_to_ui_waiter(&ui_waiting);
                    }
                    (acc, reports)
                };
                // Report the handback (term lock released; the timeline is a
                // strict leaf, taken here with no other lock held).
                if let (Some(h), Some(e)) = (&handback, edge) {
                    let payload = crate::foreground_handback::record_restored(
                        &mut timeline.lock().unwrap_or_else(|p| p.into_inner()),
                        e,
                        &h.reverted,
                        h.bytes.len(),
                    );
                    aterm_log::info!("session {id}: foreground handback: {payload}");
                }
                // The dev `card-seen` probe (`holder_image::DEV_ROWS` builds
                // only): while the holder image is a drawer whose card has not
                // been seen, a matching family and known version on screen is
                // recorded. This does not prove who wrote those cells, nor
                // distinguish stale same-build cards. At most
                // `CARD_PROBE_BATCHES` batches per snapshot token.
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
                // Report a manual reset the same way (term lock released, the
                // timeline a strict leaf), then answer whoever asked.
                if let Some((req, report, _)) = &manual_run {
                    crate::manual_reset::record(id, &timeline, req.source, report);
                    if let Some(reply) = &req.reply {
                        let _ = reply.try_send(report.clone());
                    }
                }
                // Wake the compression worker once the backlog passes the signal
                // point; a token already queued on the capacity-1 channel means a
                // drain is pending. No worker: the reader already drained inline.
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
                    // Never written inline (see `REPLY_QUEUE_CAP`); `submit` never
                    // blocks and drops the reply on a full lane.
                    let _ = reply_tx.submit(resp);
                }
                // The reports the engine sent unasked, behind this burst's replies:
                // the console host may be waiting for one of those (its cursor
                // query after a resize), and a report waits for the ConPTY resize
                // guard (`ReplyLane::submit_report`). Recorded like a reply: its
                // instant on the spine, never its bytes.
                if let Some(report) = reports {
                    let report: std::sync::Arc<[u8]> = report.into();
                    if let Some(tx) = &temporal_tx {
                        spine_mark.reply(tx);
                    }
                    let _ = reply_tx.submit_report(report);
                }
                // The batch's coalesced output wake (`output_wake`); `woke` is
                // also the cast timeline's stamp for a burst no read arrived in
                // (a manual reset's).
                let woke = Instant::now();
                output_wake(&mut cadence);
                // The launch mark's screen, read with the term lock released and
                // after the replies and the wake: no pixel depends on it. The copy
                // and the count were taken in the mark's hold; until this read the
                // mark says `pending`, which no supervisor answers under.
                if let Some(unread) = launch_unread {
                    launch.read_screen(unread);
                }
                // Cast and byte-fanout taps: program output only; `take_response()`
                // replies are the terminal's own bytes and are not `"o"` events. One
                // heap copy of the burst, shared via `Arc`. Not recording, the taps
                // make it here, after the wake, because nothing on screen depends
                // on it. While temporal records they reuse `spine_copy`, made
                // before the first hold so that no hold copies.
                //
                // The recorder is always on (`cast` / `cast frames` serialize
                // whatever the session produced, with no arming step), so it has
                // no kill switch. Order is kept by the burst's `cuts`, not by
                // record time. A handback's synthesized bytes ride the taps at the
                // cut, and a manual reset's bytes lead the burst: either splice is
                // a copy made here.
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
                // The reader records the burst itself, under the recorder's own
                // lock, never taken under `term_lock` and never held long by
                // anyone (a push, or a snapshot's chunk refcounts), so no burst
                // is dropped. An empty burst pushes no event and reaches no
                // subscriber; one a hold noted (a manual reset with no bytes)
                // still hands the recorder its hold count, so `cast drift`
                // never waits for it. `emitted` walked the same bytes in the
                // same order.
                debug_assert_eq!(emitted, burst.len(), "cast cuts address the spliced burst");
                // The burst's one arrival reading, for both taps: a manual
                // reset's spliced bytes have none of their own and take the
                // wake's.
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
                // The host's leave of the dead app's screen, last and outside the
                // read's hold: its `?1049l` arrives on a later read and leaves ours
                // there, with the prompt the shell drew on that screen carried along.
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
                // Block on the master or this session's wake pipe. `Wake` means
                // `Session::drop` asked us to stop even though the master has not
                // hit EOF (an orphaned child may hold the slave). Exit quietly (no
                // `Wake::Exit`); dropping this closure releases the reply writer's
                // `SinkWriter`, which owns and closes the master.
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
                    // Never produced here: this reader passes no process watch.
                    aterm_pty::ReadOutcome::Proc => continue,
                    // `Exited` is never produced here: the child's exit EOFs the read.
                    aterm_pty::ReadOutcome::Eof | aterm_pty::ReadOutcome::Exited => {
                        // The PTY closed on its own (shell/`-e` exited): the main
                        // thread closes only this session's tab, and exits the app
                        // only if it was the last (honoring `--hold`).
                        let _ = pty_closed_at.set(Instant::now());
                        let _ = post(Wake::Exit {
                            session: id,
                            window,
                        });
                        break;
                    }
                    // Top the buffer up past the first ~1 KiB kernel chunk (macOS tty
                    // outq) before processing, so lock/tap/wake costs are paid per
                    // ~64 KiB batch. Interactive bursts return on the first quiet poll.
                    aterm_pty::ReadOutcome::Data(n) => {
                        let read_at = Instant::now();
                        let mut filled = aterm_pty::drain_more(master, &mut buf, n);
                        let since_epoch = |at: Instant| {
                            u64::try_from(at.saturating_duration_since(lat_epoch).as_nanos())
                                .unwrap_or(u64::MAX)
                        };
                        let read_ns = since_epoch(read_at);
                        latest_output_activity_ns.store(read_ns.max(1), Ordering::Release);
                        // A held stream's read gathers what follows it before it is
                        // fed (`begin_held_gather`): a line-per-call writer then
                        // costs this reader one batch per hold, not one per line,
                        // and its lines still reach the pass the hold owes them.
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
                // ConPTY has no foreground process group: no handback.
                ingest(&buf[..r.0], None, 0, None, Some(r.1));
            }
            // Unix parse loop: batches from the gather stage. Data batches have
            // left the kernel queue, so they are always processed; the gather
            // enforces stop before its next read, and the channel always ends with
            // Eof/Wake or close, so `recv()` needs no timeout for that.
            //
            // The one timeout is the crash-restore journal's, and only while its
            // session has tail past its keyframe (`SessionJournal::dirty`): after
            // K1's quiet the burst is keyframed (`on_quiet`), and the restore of an
            // idle session replays nothing. A clean session (or one with no journal)
            // blocks as before and wakes zero times while idle.
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
                    // Every batch before this one is ingested: the parser holds
                    // what the program wrote while no reader ran. Ending the
                    // park rings the session's watchers, so a parked wait takes
                    // up its clocks at the resume.
                    Ok(GatherMsg::CaughtUp) => {
                        if park.0.caught_up(park.1) {
                            watchers.ring_watchers();
                        }
                    }
                    // A byte-free edge (card engine §3.3): an empty batch, so
                    // its publication and cut land before the next byte.
                    Ok(GatherMsg::Edge(edge, holder, image)) => {
                        ingest(&[], Some(edge), holder, image, None);
                    }
                    Ok(GatherMsg::Data(batch, len, edge, holder, image)) => {
                        ingest(&batch[..len], edge, holder, image, None);
                        // Recycle the buffer; a no-op after the gather exits.
                        let _ = free_tx.try_send(batch);
                        // Ingest done: only now does this batch stop counting as
                        // parse work for the gather bridge's idle cutoff.
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
                    // The shell exited before the master's EOF: the same exit,
                    // but what a job it left behind still writes keeps arriving.
                    Ok(GatherMsg::Exited) => {
                        let _ = pty_closed_at.set(Instant::now());
                        let _ = post(Wake::Exit {
                            session: id,
                            window,
                        });
                    }
                    Ok(GatherMsg::Wake) | Err(_) => break,
                    // A manual reset between two batches (`crate::manual_reset`):
                    // handed to `ingest` with an empty batch. `0` is "unknown
                    // holder" — a reset only clears evidence, so nobody is
                    // credited.
                    Ok(GatherMsg::Reset(req)) => {
                        pending_reset.set(Some(req));
                        ingest(&[], None, 0, None, None);
                    }
                    // A host resize or settings change with no output to follow:
                    // the journal records it now (`crate::crash_restore`).
                    Ok(GatherMsg::Journal) => {
                        if let Some(j) = &journal {
                            j.on_nudge(&term);
                        }
                    }
                }
            }
            // This stage takes no more resets: close the lane now rather than
            // when the gather's guard drops, so a request that races the end
            // runs directly instead of waiting out its timeout.
            #[cfg(unix)]
            reset_lane.close_if(lane_generation);
            // Windows: this thread owns the wake pipe's read end and closes it on
            // exit (`Session::drop` owns the write end); a no-op for `-1`. On unix
            // the gather owns it.
            #[cfg(windows)]
            aterm_pty::close_fd(wake_rd);
        },
    )
    .map_err(|error| format!("spawn PTY reader: {error}"))?;
    #[cfg(unix)]
    start_gather()?;
    Ok(parse)
}

/// The term-lock hold width for one ingest slice of a PTY burst: fine (8 KiB)
/// while a keystroke is waiting to echo (`pending`, the lock-free
/// [`crate::input_hint::pending`] signal), otherwise a wider but capped slice
/// (`IDLE_CHUNK`), so a flood pays fewer lock round trips while a key arriving
/// mid-hold waits at most the cap. Re-evaluated per hold. `pending` is asked
/// only when the remainder exceeds the fine slice.
fn ingest_chunk_width(pending: impl FnOnce() -> bool, remaining: usize) -> usize {
    const PROCESS_CHUNK: usize = 8 * 1024;
    // The idle width is capped because `pending` is sampled per hold: an
    // unbounded remainder would make a key arriving just after a whole-burst
    // hold began wait out the entire batch.
    const IDLE_CHUNK: usize = 16 * 1024;
    if remaining <= PROCESS_CHUNK {
        remaining
    } else if pending() {
        PROCESS_CHUNK
    } else {
        IDLE_CHUNK.min(remaining)
    }
}

/// Arm the output-to-present measurement for a burst about to be parsed.
///
/// `compare_exchange` from 0: the first unpresented edge wins, so a burst
/// spanning several reads or batches is measured from where the terminal first
/// fell behind. The present path consumes the stamp with `swap(0)`. `now_ns` is
/// non-zero because 0 means "no sample".
///
/// Called before the engine sees the bytes, so the parse and term-lock wait,
/// which grow when the terminal falls behind, are inside the measurement.
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

    /// First edge wins: several bursts before one present measure from the
    /// earliest, and the present's `swap(0)` re-opens the stamp.
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
        // The present path's consume.
        assert_eq!(stamp.swap(0, Ordering::Relaxed), 10);
        stamp_output_arrival(&stamp, 40);
        assert_eq!(
            stamp.load(Ordering::Relaxed),
            40,
            "the next edge arms freely once a present has booked the previous one"
        );
    }
}

/// The age at which a posted `Wake::Output` still waiting for its handler is
/// booked late (`metrics` `output_wake_late`, a debug-level note), and the delay
/// after which an idle event loop turns once more to look at an armed latch.
/// Age alone never proves a wake lost: a wake posted during a long frame is
/// old but still queued. Loss is judged on evidence by [`judge_output_wake`].
pub(crate) const WAKE_LATCH_EXPIRY_NS: u64 = 100_000_000;

/// Bit 63 of an armed latch word: the wake the arm stands for has been POSTED,
/// and the rest of the word is a `lat_epoch` stamp read AFTER the post returned.
/// Clear: the reader has armed and is still posting. The stamp lives below the
/// four flag bits ([`WAKE_LATCH_STAMP`]), so no flag ever collides with a time.
pub(crate) const WAKE_LATCH_SENT: u64 = 1 << 63;

/// Bit 62 of a latch word: the UI thread HOLDS the latch after a pass on a
/// streaming session ([`output_latch_after_pass`]), and the stamp is the
/// `lat_epoch` instant the hold falls due. Readers post nothing on a held latch:
/// the first chunk fed after the pass marks it [`WAKE_LATCH_DIRTY`], and the
/// hold's release ([`release_output_hold`], at the deadline `about_to_wait`
/// folds) runs that session's pass. Never set with [`WAKE_LATCH_SENT`], so the
/// lost-wake watchdog never judges a hold.
pub(crate) const WAKE_LATCH_HELD: u64 = 1 << 62;

/// Bit 61 of a held latch word: a reader fed the engine after the pass that
/// took the hold, so its release owes the session a pass.
pub(crate) const WAKE_LATCH_DIRTY: u64 = 1 << 61;

/// Bit 60 of an armed latch word: the session is streaming ([`PostCadence`]),
/// so the pass this wake buys takes a hold instead of clearing the latch.
pub(crate) const WAKE_LATCH_STREAMING: u64 = 1 << 60;

/// The stamp bits of a latch word: `lat_epoch` nanoseconds, which reach the
/// lowest flag bit after 36 years.
pub(crate) const WAKE_LATCH_STAMP: u64 = WAKE_LATCH_STREAMING - 1;

/// The shortest interval between two `Wake::Output` passes for one streaming
/// session. A program that writes line by line (a pwsh pipeline, cmd's `type`, a
/// build log) gives its reader one line per read, and each read used to buy its
/// own loop turn: 19,133 wakes for 20,000 lines of `Get-Content`, about 100 µs of
/// aterm CPU per line. Once a session streams ([`STREAM_AFTER_NS`]), each pass
/// takes a hold this long, and output fed under it waits for the hold's release,
/// at most this long. Below the half-frame floor that paces a frame at up to
/// 120 Hz (`content_pace_floor`), so a frame rarely waits on a hold.
pub(crate) const OUTPUT_CADENCE_NS: u64 = 4_000_000;

/// How long a burst of posts (each under [`OUTPUT_CADENCE_NS`] after the one
/// before) must have run before its session counts as streaming: one 60 Hz
/// frame. A key's echo is never held: under OpenConsole a PSReadLine echo is
/// about seven reads in 0.2 ms (cursor off, text, colours, cursor on), and a
/// held tail would wait for its release. A flood is held from its 17th
/// millisecond on, and through any stall of its writer shorter than this
/// ([`PostCadence`]).
pub(crate) const STREAM_AFTER_NS: u64 = 16_000_000;

/// How many posts a burst makes before its session counts as streaming even
/// short of [`STREAM_AFTER_NS`]: a typed echo is about seven reads (above), a
/// burst of this many passes is a flood. Measured 2026-10-20 (WIN-1), ten tabs
/// flooding behind a window's idle front tab: a flood whose writer kept stalling
/// for 4 ms and more before its burst had run a frame restarted it each time
/// (2,350 restarts in 8.4 s), and spent 16 ms of every restart buying a pass per
/// read, about 160 of them: 374,000 `Wake::Output`s and 278 ms/s of UI-thread
/// work, where an unstalled run made 1,200 to 3,000 and 35 ms/s.
pub(crate) const STREAM_AFTER_POSTS: u32 = 16;

/// The event loop's turn state as the PTY readers see it: the `lat_epoch`
/// stamp of the `about_to_wait` that last put the loop to sleep, or `0` while a
/// turn is running (from `new_events` or any `window_event`; a macOS `drawRect:`
/// can run outside the `new_events`/`about_to_wait` bracket) and before the
/// first turn ends.
///
/// Used by [`gated_output_wake`] to tell a posted `Wake::Output` stranded with
/// its loop wake-up, which the watchdog ([`OutputWakeWatch::pass`]) never sees: it
/// only sees latches an `about_to_wait` has seen. A latch stamped after
/// `since_ns()`, with the loop still idle and older than the expiry, is such a
/// wake; one queued behind a long frame never is (the loop is not idle).
#[derive(Debug, Default)]
pub(crate) struct LoopIdle(AtomicU64);

impl LoopIdle {
    /// A turn is running (`new_events`, `window_event`): no re-post.
    pub(crate) fn turn_began(&self) {
        self.0.store(0, Ordering::Relaxed);
    }

    /// `about_to_wait` is done and the loop is about to sleep; `pass_ns` is the
    /// stamp the watchdog judged this turn at (every latch it saw was stamped
    /// before it, or was seen unsent).
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
    /// Nothing posted: the latch was armed (the chunk rides the wake already
    /// posted), another reader won the arm, or the send failed.
    Coalesced,
    /// A fresh wake on the latch's clear->armed edge.
    Posted,
    /// A stranded wake re-posted (see [`wake_stranded`]): not a heal — the
    /// watchdog never saw it — and counted apart (`output_wake_reposts`).
    Reposted,
    /// Nothing posted: the UI thread holds the latch ([`WAKE_LATCH_HELD`]) and
    /// the chunk left it dirty, so the hold's release runs the pass.
    Held,
}

/// Whether an armed latch `word` is a wake stranded with its loop wake-up:
/// sent, stamped after the loop last went idle (`idle_since_ns`, `0` = a turn is
/// running), and older than [`WAKE_LATCH_EXPIRY_NS`]. See [`LoopIdle`]. A wake
/// possibly queued behind a long frame, one an `about_to_wait` already saw, and
/// an unsent arm never qualify.
pub(crate) fn wake_stranded(word: u64, idle_since_ns: u64, now_ns: u64) -> bool {
    let at = word & WAKE_LATCH_STAMP;
    word & WAKE_LATCH_SENT != 0
        && idle_since_ns != 0
        && idle_since_ns < at
        && now_ns.saturating_sub(at) > WAKE_LATCH_EXPIRY_NS
}

/// Hand one burst the engine has taken to the live byte fan-out (`bytes`
/// subscribers: one refcount bump each, never blocking the reader) with its
/// arrival `at_us` on `metrics::now_us`'s clock, then `wake` the session's
/// watchers. In that order: a watcher the wake reaches reads an engine that
/// took the burst and a `bytes` queue that holds it, and reaches its content
/// and events without the main thread's turn. An empty burst reaches no queue
/// but still wakes (a manual reset can change the engine with no bytes of its
/// own).
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

/// [`gated_output_wake`] for a test of a post that refuses (a warm spare's,
/// `crate::spare::SpareLink::decide`).
#[cfg(test)]
pub(crate) fn gated_output_wake_for_test(
    flag: &AtomicU64,
    now_ns: u64,
    send: impl FnOnce() -> bool,
) -> WakeGate {
    gated_output_wake(flag, now_ns, 0, false, send, || now_ns)
}

/// Edge-triggered `Wake::Output` gate: invoke `send` only when the latch goes
/// clear-to-armed, so a flood queues at most one user event per handler pass.
/// The handler clears the latch before its work, so a chunk processed after the
/// clear re-arms and the final burst is never lost; the CAS is an RMW, so a
/// concurrent clear is never read stale. `Relaxed` suffices: grid content is
/// synchronized by the term mutex. A failed send (headless, no event loop) must
/// reset the latch, or the arm would suppress sends until the watchdog cleared it.
///
/// Two-phase arm: the arm stores `now_ns` without [`WAKE_LATCH_SENT`]; once
/// `send` returns, the word is re-stamped with `sent_ns()` and the bit, which
/// lets the watchdog tell a lost wake from a late one. `streaming`
/// ([`PostCadence::streaming`]) rides both words as [`WAKE_LATCH_STREAMING`], so
/// the pass takes a hold.
///
/// A HELD latch ([`WAKE_LATCH_HELD`]) posts nothing: the chunk marks it dirty
/// (one CAS per hold; a held word that is already dirty is a load), and the
/// hold's release runs the pass. A CAS that loses to the UI thread releasing or
/// re-taking the hold looks again, so the chunk is always covered by a dirty
/// mark or a fresh post.
///
/// The reader never re-sends a wake that may be queued: an armed latch
/// coalesces however old unless [`wake_stranded`]. A wake the loop has turned
/// past is the watchdog's to heal ([`OutputWakeWatch::pass`]); a stranded one is
/// re-posted here while output streams, and one stranded on a burst's last chunk
/// waits for the loop's next turn.
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
                // Released or re-taken meanwhile: look again.
                Err(now) => {
                    word = now;
                    continue;
                }
            }
        } else if wake_stranded(word, idle_since_ns, now_ns) {
            (word, WakeGate::Reposted)
        } else {
            return WakeGate::Coalesced; // armed: this chunk rides the wake already posted
        };
        // CAS from the observed word so concurrent readers arm exactly once, and a
        // handler clear (or a watchdog heal) that lands first stands. A hold that
        // lands first is looked at again: this chunk must leave it dirty.
        match flag.compare_exchange(from, arm, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break posted,
            Err(now) if now & WAKE_LATCH_HELD != 0 => word = now,
            Err(_) => return WakeGate::Coalesced,
        }
    };
    if send() {
        // CAS from the arm: a handler that already consumed the event cleared
        // the latch (or took a hold), and that must stand.
        let stamp =
            (sent_ns() & WAKE_LATCH_STAMP).max(arm & WAKE_LATCH_STAMP) | WAKE_LATCH_SENT | flags;
        let _ = flag.compare_exchange(arm, stamp, Ordering::Relaxed, Ordering::Relaxed);
        posted
    } else {
        let _ = flag.compare_exchange(arm, 0, Ordering::Relaxed, Ordering::Relaxed);
        WakeGate::Coalesced
    }
}

/// A reader's memory of its own output (`lat_epoch` ns): its `Wake::Output`
/// posts and the chunks it fed under a hold, which says whether its next post
/// carries [`WAKE_LATCH_STREAMING`]. A BURST is a run of that output, each under
/// [`OUTPUT_CADENCE_NS`] after the one before; a post is streaming once its burst
/// has run [`STREAM_AFTER_NS`] or made [`STREAM_AFTER_POSTS`] posts. A quiet spell
/// of a cadence ends a burst that has not streamed yet, and a quiet frame
/// ([`STREAM_AFTER_NS`]) one that has.
///
/// A held latch takes no posts, so a hold chain ends at a clean release: a
/// cadence with no output. On a busy Mac that is the writer descheduled for 4 ms,
/// not the end of its stream: a `seq` run beside a build (a default-band process,
/// as every program a shell starts is) stalls that long dozens of times a second.
/// Ended at each stall, the stream bought a pass per read for the next frame
/// again. A stream that resumes within a frame now posts as streaming at once:
/// its first chunk still gets its pass at once, and the chunks behind it a hold.
/// The window is no longer than a frame because a key's echo just after a stream
/// continues it, and so does each next key inside the window: one as long as a
/// fast typist's gap between keys (tens of milliseconds) would keep a finished
/// stream going under the typing and hold each echo's reads behind its first.
/// Measured 2026-10-19 (CTL-1), ten tabs flooding beside eight default-band
/// spinners (medians of 4): 143,000 `Wake::Output`s and 45 ms/s of UI-thread
/// work before, 40,000 and 20 ms/s after, the flood done in 13.45 s instead of
/// 14.07; on a quieter machine (medians of 10), 6,300 and 12.1 ms/s before,
/// 3,500 and 10.7 after.
///
/// A burst that has not streamed yet still ended at a cadence's stall, and a
/// writer that stalls that often never ran a whole frame: each restart bought a
/// pass per read for up to a frame again, about 160 passes when the UI thread
/// answers every post at once (a window's, behind its idle front tab). Its
/// [`STREAM_AFTER_POSTS`]th post ends that: the burst streams from there, so a
/// restart costs at most that many passes, and a typed echo (about seven reads)
/// still never streams.
///
/// A dense burst is often a command's output inside a frame (a dozen posts are
/// the shell's own marks and the holder's change), and its prompt follows it,
/// within the hold the burst's last pass took or after a stall that streams.
/// Held, the prompt waited for a release: measured 2026-10-20 (WIN-1 review,
/// windowed, medians of 20, load 5-12), 40 lines 300 µs apart and the prompt
/// 6 ms after them reached its pass 12.51 ms after the last line instead of
/// 8.62. A prompt comes after a foreground edge (the program's end), and so
/// does a new program's first output, so an edge ends the burst and breaks the
/// hold that stood for it ([`foreground_edge`](Self::foreground_edge)): the
/// prompt's reads each get their pass at once, after any output, a frame long
/// or a flood. A loop of short programs pays a pass per read for it, as before
/// CTL-1 (an edge at every program's start and end).
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct PostCadence {
    /// The last post or held chunk (`0` = none yet).
    last_ns: u64,
    /// The first post of the burst the last one belongs to.
    burst_from_ns: u64,
    /// The posts the burst has made so far.
    posts: u32,
}

impl PostCadence {
    /// Whether output at `now_ns` continues the current burst.
    fn continues(&self, now_ns: u64) -> bool {
        if self.last_ns == 0 {
            return false;
        }
        let quiet = now_ns.saturating_sub(self.last_ns);
        let streamed =
            self.dense() || self.last_ns.saturating_sub(self.burst_from_ns) >= STREAM_AFTER_NS;
        quiet < OUTPUT_CADENCE_NS || (streamed && quiet < STREAM_AFTER_NS)
    }

    /// Whether the burst has made [`STREAM_AFTER_POSTS`] posts.
    fn dense(&self) -> bool {
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

    /// A chunk fed at `now_ns` left a held latch dirty ([`WakeGate::Held`]): the
    /// stream goes on under its hold, with no post.
    pub(crate) fn held(&mut self, now_ns: u64) {
        self.last_ns = now_ns;
    }

    /// The batch about to be fed carries a foreground edge (a program started
    /// or ended): its output is not the burst's, which ends here, and a hold on
    /// `latch` (the session's output latch) is broken, from its held word to `0`
    /// by CAS, looking again when the CAS loses to a reader's DIRTY mark. The
    /// batch's post then arms the latch afresh and goes out at once; its pass
    /// reads every chunk fed under the broken hold too, and the hold's release
    /// finds the latch no longer held ([`HoldRelease::NotHeld`]).
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

/// The latch word a `Wake::Output` pass stores before its work, from the
/// `word` it found, so a chunk fed after the store posts again or leaves the
/// hold dirty. A streaming session's pass ([`WAKE_LATCH_STREAMING`] on its
/// wake, or a pass that ran on a hold: its release, or another wake meanwhile)
/// takes a hold due [`OUTPUT_CADENCE_NS`] after `now_ns`; any other pass clears
/// the latch, so the session's next output posts at once.
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
    /// Held until `due_ns` (`lat_epoch` ns): fold a deadline there.
    Pending { due_ns: u64 },
    /// Due, and nothing was fed since the pass that took it: cleared, so the
    /// session's next output posts at once.
    Released,
    /// Due and dirty: the caller runs the session's pass, which takes the
    /// hold again ([`output_latch_after_pass`]).
    Due,
}

/// Release one session's output hold if it is due at `now_ns`. A clean hold is
/// cleared by CAS; one a reader dirtied meanwhile is looked at again, so a
/// chunk fed while it was held is never dropped with it.
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

/// A read at least this big is fed as it is, and a held gather
/// ([`gather_held_output`]) stops once it holds this much: the costs a reader
/// pays per batch (the term-lock hold and the engine's per-call work, the cast
/// and byte taps, the wake gate, the compress signal, a blocking read's
/// wake-up) are then paid per 16 KiB, as a bulk writer's are.
#[cfg(any(windows, test))]
pub(crate) const HELD_GATHER_ENOUGH: usize = 16 * 1024;

/// How long before its hold falls due a held gather is fed, so its bytes are
/// in the engine when the hold's release runs the session's pass.
#[cfg(any(windows, test))]
pub(crate) const HELD_GATHER_MARGIN_NS: u64 = 500_000;

/// The shortest held gather. A read near or past its hold's due instant (the
/// release is about to run, or is late) still gathers this long, so the reads
/// around each release are not fed one by one.
#[cfg(any(windows, test))]
pub(crate) const HELD_GATHER_MIN_NS: u64 = 1_000_000;

/// How often a held gather looks at the pipe while it waits: what a fast
/// writer puts there is collected well before the pipe's 64 KiB would fill and
/// stall it.
#[cfg(any(windows, test))]
pub(crate) const HELD_GATHER_STEP_NS: u64 = 1_000_000;

/// How far past its request a held gather's wait is taken to wake, so the
/// gather asks that much less and ends on time. Windows' high-resolution wait
/// (`std::thread::sleep`) overshoots by about half a millisecond: measured
/// 2026-10-18 on the stock 1.97.1 build, 200 asks each, 1 ms woke after 1.53 ms
/// at the median (1.84 at p99), 0.5 ms after 1.01 and 3.5 ms after 4.01. A
/// gather with less than this left ends instead of waiting.
#[cfg(any(windows, test))]
pub(crate) const HELD_GATHER_WAIT_SLACK_NS: u64 = 500_000;

/// Whether a reader that has just read `filled` bytes at `now_ns` gathers more
/// before it feeds them ([`gather_held_output`]), and until when (`lat_epoch`
/// ns). A writer that makes one call per line (pwsh's `Get-Content`) hands the
/// reader one line per read, and each fed read pays the reader's per-batch
/// costs: 200,000 lines cost about 7.5 s of aterm CPU where `cat` of the same
/// bytes costs 0.3 s.
///
/// Only while the UI thread HOLDS the session's latch ([`WAKE_LATCH_HELD`]):
/// its pass reads the engine when the hold falls due and not before, so bytes
/// fed then show no later than bytes fed now. Never for a read of
/// [`HELD_GATHER_ENOUGH`] or more, and never while a key waits for its echo
/// (`typing`, [`crate::input_hint::pending`]). The gather ends
/// [`HELD_GATHER_MARGIN_NS`] before the hold is due, and runs at least
/// [`HELD_GATHER_MIN_NS`] and at most [`OUTPUT_CADENCE_NS`].
///
/// A gather marks the hold DIRTY as it begins, before its bytes are fed, so the
/// hold's release runs the session's pass and holds again even when the
/// gather is fed after it (a late wake-up): the hold chain never breaks into a
/// pass per read. Bytes fed after that release mark the next hold dirty
/// through [`gated_output_wake`], as every chunk does.
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
            // Released, re-taken or dirtied meanwhile: look again.
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

/// Gather a held session's output into `buf[..filled]` until `until_ns`
/// ([`begin_held_gather`]): `top_up` collects what the pipe already holds
/// without blocking, then the gather waits a step ([`HELD_GATHER_STEP_NS`]),
/// asking [`HELD_GATHER_WAIT_SLACK_NS`] short of `until_ns` for the wait's
/// overshoot, and looks again. It ends once less than that slack is left before
/// `until_ns`, once it holds [`HELD_GATHER_ENOUGH`] (or the buffer is full), or
/// when `go_on` says no (a key now waits for its echo, or the reader is told to
/// stop), and returns the fill. Every byte it read is fed: it never ends
/// between a read and its return.
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
    /// The wake was posted before `drained_through_ns`, a user-event drain ran
    /// after that instant, and the latch is still armed: no handler consumed
    /// the event. Heal it.
    Lost { age_ns: u64 },
    /// Outstanding but not proven lost: queued behind the main thread's current
    /// turn, or still being posted. Look again at `recheck_ns` (never in the
    /// past); `late` books a wake older than [`WAKE_LATCH_EXPIRY_NS`] (a
    /// measurement, never an act).
    Waiting { recheck_ns: u64, late: bool },
}

/// The watchdog's verdict on one latch word, on evidence rather than age.
///
/// `drained_through_ns` is the `lat_epoch` stamp of the previous
/// `about_to_wait`. Every winit platform delivers, between two `about_to_wait`s,
/// every user event already queued when the first ran (macOS drains the proxy
/// channel before `AboutToWait`; X11/Wayland dispatch it before the iteration;
/// Windows emits `AboutToWait` only once its queue is empty). A wake stamped
/// sent before that instant was delivered, and delivery clears the latch, so a
/// latch still holding that stamp is `Lost`; anything else is waiting. An unsent
/// arm (the reader is mid-post) rechecks after its own age again (at least one
/// expiry), so a stalled post is looked at ever more rarely.
///
/// A sent wake rechecks one expiry after this pass (`now_ns + expiry`), never
/// earlier than `stamp + expiry + 1` and never in the past: a wake that waited
/// out a long frame is still queued and turns the loop itself, and a past
/// deadline would trip the scheduler's stale-arm backstop
/// (`deadline_heal::DeadlineHealer::heal`) on every long frame.
///
/// A held latch ([`WAKE_LATCH_HELD`]) is `Clear`: no wake stands behind it, and
/// its release has its own deadline (`OutputCadence`).
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
        // Backs off with the arm's age: a reader that stalls between its arm
        // and its post costs an idle loop a logarithmic number of looks, not a
        // periodic wake-up.
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
    /// `lat_epoch` ns of the previous pass: every wake posted before it has been
    /// through a user-event drain since (see [`judge_output_wake`]). `0` before
    /// the first pass, so nothing is presumed lost until a drain has run.
    drained_through_ns: u64,
    /// `(session, latch word)` pairs already booked late: one book per wake,
    /// however many turns it waits.
    late_booked: Vec<(u64, u64)>,
}

/// One watchdog pass's outcome.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OutputWakePass {
    /// Latches proven lost and cleared this pass: the caller repaints.
    pub(crate) healed: u32,
    /// Wakes newly booked late this pass (no action taken).
    pub(crate) late: u32,
    /// The earliest recheck any still-outstanding wake asks for.
    pub(crate) recheck_ns: Option<u64>,
}

impl OutputWakeWatch {
    /// Judge every session's latch at `now_ns` (this `about_to_wait`'s stamp),
    /// heal the ones proven lost (CAS from the observed word, so a concurrent
    /// handler clear or reader re-stamp is never clobbered), book the late ones
    /// once, and record `now_ns` as the next pass's drained-through instant.
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
    /// Register a session's live handle in the process-wide registry. The
    /// `term`/`sink`/`ctx` `Arc`s are shared with the owning `Session`, so
    /// cross-session reads are zero-copy. Called at every spawn seam;
    /// deregistered when its tab closes.
    ///
    /// The handle is registered `Spawning`: engine, master and sink exist (input
    /// and reads are safe; early bytes buffer in the kernel), but the reader has
    /// not confirmed its first iteration. The reader flips it to `Alive` by
    /// posting `Wake::Ready` ([`spawn_pty_reader`]), handled via
    /// [`session_store::SessionStore::mark_alive`]; a slow shell stays `Spawning`
    /// and addressable without blocking the GUI.
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
            // An adopted shell with a frozen PATH is recorded where the
            // managed-current row counts it and the next handoff carries it.
            if session.frozen_path {
                registry.mark_frozen_path(session.id);
            }
            // So is a shell with the re-key channel: the next handoff carries the
            // fact, so its successor can re-key it (`shell_rekey`).
            if session.rekey_channel {
                registry.mark_rekey_channel(session.id);
            }
            // So is a shell with an integration loader, for its successor to point
            // at its own body, and one adopted from before loaders, which `status
            // integration_rev=` names `frozen` (`shell_body`).
            if session.body_loader {
                registry.mark_body_loader(session.id);
            }
            if session.integration_frozen {
                registry.mark_integration_frozen(session.id);
            }
            // And the history its update handoffs could not carry: `status` says it
            // (`history_lost=`) and the next handoff carries it on.
            if let Some(history) = session.handoff_history.as_ref() {
                registry.add_history_lost(session.id, history.lost);
            }
            drop(registry);
        });
        // Sibling discovery: publish this session's graph entry (sid → our
        // instance socket) so an `@<sid>` arriving at another same-uid instance
        // is forwarded here. A no-op until the control socket is bound.
        crate::proxy::publish_session(&session.ctx.self_id, &session.ctx.nonce);
    }
}

/// The directory holding aterm's co-located CLI tools (`aterm-ctl`, `atpkg`)
/// when running from a macOS .app bundle (`Contents/MacOS`), put on child
/// sessions' PATH so the bundled Help's workflows run with no install step.
/// `None` outside a bundle.
///
/// On Windows it is the running image's folder when that folder holds the
/// console image `aterm.exe` ([`windows_app_dir`]): an installed or portable
/// app folder, or a build tree's `target\<profile>`. Without it a tab's `aterm`
/// ran whichever copy the user PATH named, or none at all from a portable
/// folder (measured 2026-10-17: a scratch 0.106 build's tabs resolved `aterm`
/// to the installed 0.101 in pwsh, cmd and Git Bash).
///
/// On Linux it is the store folder `tools/install.sh` lays, when the window runs from it
/// ([`linux_store_dir`]): it holds the same aliases, `amacs` among them, and before this a
/// Linux tab had none of them on PATH (`docs/DESIGN-amacs-2026-10-19.md` §8.2).
pub(crate) fn bundle_dir(exe: Option<&std::path::Path>) -> Option<String> {
    if cfg!(windows) {
        return windows_app_dir(exe?, |path| path.is_file());
    }
    if cfg!(target_os = "linux") {
        return linux_store_dir(exe?, |path| std::fs::canonicalize(path).ok());
    }
    let dir = exe?.parent()?;
    // Component-wise suffix match: only a real `<Name>.app/Contents/MacOS` layout
    // qualifies (the shape codesign seals), so this is inert for dev/CI binaries.
    if !dir.ends_with("Contents/MacOS") {
        return None;
    }
    dir.to_str().map(str::to_owned)
}

/// [`bundle_dir`]'s Windows rule: the folder of `exe` when `is_file` finds the
/// console image `aterm.exe` in it, so a tab's `aterm`, `aterm-ctl` and `atpkg`
/// are this build's own — whichever image runs the window (`aterm-gui.exe`,
/// a build tree's `aterm-windowed.exe`, or `aterm.exe --headless`). A folder
/// without the console image puts nothing on PATH: nothing in it answers a
/// bare `aterm` at a prompt. `is_file` is injected so the rule is tested
/// without laying out a folder.
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

/// [`bundle_dir`]'s Linux rule: the folder of `exe` when it is the store `tools/install.sh`
/// lays (`~/.local/lib/aterm/bin`), which holds the argv0 aliases beside the one `aterm`
/// binary: `exe` is named `aterm` and the `aterm-ctl` beside it is an alias of it, a link
/// `resolve` takes to the same file. The rule is atpkg's ([`atpkg::hooks::store_dir_of`]),
/// shared with the pass that lays an alias missing there, so the folder on PATH is the one
/// that pass completes. A build tree's `target/<profile>`, whose `aterm-ctl` is a dev binary
/// of its own, puts nothing on PATH, as off a bundle on macOS. `resolve` is injected so the
/// rule is tested without laying out a folder.
pub(crate) fn linux_store_dir(
    exe: &std::path::Path,
    resolve: impl Fn(&std::path::Path) -> Option<std::path::PathBuf>,
) -> Option<String> {
    atpkg::hooks::store_dir_of(exe, resolve)?
        .to_str()
        .map(str::to_owned)
}

/// The two environment variables that make a no-terminal `sudo` reach aterm's
/// askpass helper (`docs/DESIGN-sudo-askpass-2026-10-15.md` §3.1). Pure over
/// its inputs so it is unit-tested with no bundle and no sandbox:
///
/// - `bundle`: [`bundle_dir`] of the running executable — `Some` only inside a
///   real `.app`, so a dev `targo run` sets nothing.
/// - `contained`: the session runs under containment. Then nothing is set: the
///   Seatbelt profile refuses setuid `sudo` and the window brokers nothing for
///   a sandboxed shell (`ATERM_DESIGN` §5.6).
/// - `inherited_askpass` / `inherited_display`: the launch environment's values.
///
/// Rules. `SUDO_ASKPASS` is set to this bundle's `aterm-askpass` when unset, or
/// when the inherited value is itself an `aterm-askpass` (an enclosing aterm's,
/// replaced with this instance's); a user's own helper is kept (not emitted, so
/// `build_child_env` leaves the inherited value in place). `DISPLAY` is set to
/// empty only when unset — `sudo`'s no-tty fallback tests presence, not value,
/// and a real (XQuartz) `DISPLAY` is kept. Empty off macOS / no bundle /
/// contained.
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

/// The one `("PATH", value)` pair the baseline `env_add` carries, composed
/// because `aterm_pty::build_child_env` applies `env_add` by key overwrite
/// (`docs/DESIGN-toolchain-reroute-2026-09-07.md` §"Reaching PATH"). Front to back:
///
/// 1. `reroute_dir`: the session-scoped stub directory for upstream Rust names
///    (`atpkg::reroute`), moved to the front: every occurrence in `inherited` is
///    removed first, so nesting and a user PATH that already names it still put
///    it first.
/// 2. `agents_dir`: `<prefix>/agents/`, holding only the shims of the agent
///    programs aterm manages (`atpkg::stub::AGENT_PROGRAMS`), with the same
///    move-to-front rule, so it outranks `~/.local/bin` and Homebrew copies
///    (`docs/DESIGN-which-copy-runs-2026-08-27.md`). Passed only when the
///    directory exists.
/// 3. `bundle_dir`: the co-located CLI tools, if given and not already present.
/// 4. `inherited`: the launch PATH, verbatim (an empty entry means "here" to a
///    POSIX shell and is kept).
/// 5. `bin_dir`: `<prefix>/bin/` ([`atpkg::session_bin_dir`]), APPENDED once and only
///    when absent — the shell hook's own placement and guard (`atpkg::hooks`), so a
///    managed tool never shadows a system one and a hook sourced later adds no second
///    copy. A process that never sources the hook (an agent, a script, a shell
///    without the integration) still runs a tool the moment its shim lands.
///
/// `None` when there is nothing to inject.
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
    // Front-inserted in this order so the final PATH reads reroute, agents, …: the
    // two name sets are disjoint, so their relative order is immaterial; that both
    // precede everything else is the point.
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

/// The managed `agents/` directory every session gets in front of its PATH,
/// created at launch if missing (one `mkdir -p`; nothing blocks the first
/// frame), so the first tab on a fresh machine already has it on PATH and finds
/// the twins atpkg lays later. An rc that prepends `~/.local/bin` can reorder
/// PATH again; the shell integration's per-prompt re-assert (sourcing
/// `~/.aterm/shell.d/00-atpkg.*`) fixes that in the tab.
///
/// Uses [`atpkg::store::Layout::ensure_agents_dir`], so a symlink or regular
/// file at `agents/` is refused. `None` when the directory could not be created
/// or was refused; the spawn then omits it and the reason goes to stderr
/// ([`atpkg::store::agents_dir_refusal_line`]).
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

/// [`atpkg_child_path`], resolved ONCE per process — for the package lane's update
/// children (the lane starts it on a thread of its own at launch, so its launch seed never
/// waits on it) and the Settings page's read-time SHADOWED rows, refreshed after every
/// pass — so neither a pass nor a page refresh runs the login shell again. A person's
/// Settings verb still asks afresh.
pub(crate) fn atpkg_child_path_once() -> &'static str {
    ATPKG_CHILD_PATH.get_or_init(atpkg_child_path)
}

/// [`atpkg_child_path_once`]'s answer, once the login shell has given it.
static ATPKG_CHILD_PATH: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// The PATH the package lane hands an update child (`PkgLane::child_path`). A routine pass
/// waits for the login shell's ([`atpkg_child_path_once`]). The FIRST RUN a person waits on
/// (`waited`, `crate::pass_is_waited_on`) never does: it takes that answer if the shell has
/// already given it, else at once the fallback the probe would fall back to
/// ([`atpkg_fallback_path`]). A heavy rc (nvm, conda, oh-my-zsh) otherwise held the first
/// ALab install by the rc's own time, up to [`LOGIN_PATH_BUDGET`].
/// An update reads PATH only for the foreign copies it would shadow and for a member a
/// system copy satisfies, and every pass reads both again: the fallback holds the places
/// such a copy measurably lives, and a copy only the login PATH finds is seen by the next
/// pass, which retires a managed twin the first run laid.
pub(crate) fn atpkg_update_path(waited: bool) -> std::borrow::Cow<'static, str> {
    update_path_with(
        waited,
        ATPKG_CHILD_PATH.get().map(String::as_str),
        atpkg_child_path_once,
        atpkg_fallback_path,
    )
}

/// [`atpkg_update_path`] over the login shell's answer as it stands (`ready`), the wait for
/// it and the fallback. Pure for the test.
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

/// The PATH the co-located atpkg children run with. A Finder-launched app
/// inherits launchd's minimal PATH, so `atpkg seed`/`atpkg update` could not see
/// foreign copies like `~/.local/bin/claude` and would disagree with a terminal.
/// The children get the login shell's PATH: one `<shell> -l -i -c 'printf %s
/// "$PATH"'` ([`path_probe_shell`]), bounded by [`LOGIN_PATH_BUDGET`], falling
/// back to the process PATH plus `~/.local/bin` (and `/opt/homebrew/bin` on
/// macOS) when the shell does not answer.
pub(crate) fn atpkg_child_path() -> String {
    login_shell_path().unwrap_or_else(atpkg_fallback_path)
}

/// [`atpkg_child_path`] when the login shell does not answer: [`fallback_child_path`] over
/// this process's PATH and home, with the managed dirs the shell hook would place
/// (`agents/` first, `bin/` last), so a fallback PATH cannot report a false shadowed row
/// for a `claude` the managed twin outranks.
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

/// How long [`atpkg_child_path`] waits for the login shell before falling back: an
/// rc file that blocks (a `read`, an `ssh-add` prompt) must not stall provisioning.
#[cfg(not(windows))]
const LOGIN_PATH_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

/// How long [`login_shell_path`] keeps collecting after the shell has answered: the
/// PATH line is already in the pipe then, so this only covers the reader's hop.
#[cfg(any(not(windows), test))]
const LOGIN_PATH_GRACE: std::time::Duration = std::time::Duration::from_millis(50);

/// The user's login-shell `PATH`, or `None` when the shell did not answer in time /
/// printed nothing that looks like one.
///
/// Always `None` on Windows, where a login shell is a POSIX concept: the `PATH` a
/// Git Bash or MSYS `bash` prints is its own `:`-separated spelling
/// (`/c/Users/…:/usr/bin`), which no native child resolves programs against, so
/// [`atpkg_child_path`] takes [`fallback_child_path`]. Asking would also start
/// that `bash` from a window with no console, and the console image would open a
/// visible console window for its whole rc chain.
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

/// The shell [`login_shell_path`] asks for its `PATH`, from the account's shell
/// (`$SHELL`, else the password database's — the spawn's own resolution): that
/// shell when its `-l -i -c` reads the person's login files and runs a POSIX
/// `printf` — zsh, bash, fish, and off macOS the `sh` family (dash is Debian's
/// and Ubuntu's `/bin/sh`) — else a default that does: `/bin/zsh` on macOS,
/// whose login rc (`~/.zprofile`) is where Homebrew's shellenv lives, and
/// `/bin/sh` elsewhere, which reads `/etc/profile` and `~/.profile` (zsh is no
/// default there and often absent; tcsh refuses `-l` beside other flags, and
/// nu, xonsh or elvish speak no POSIX).
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

// How many times this thread has entered [`login_shell_path_with`]. The Windows
// test proves `login_shell_path` asks no shell by this count, which means
// something on a box with no bash at all, where a failed spawn would also
// answer `None`.
#[cfg(test)]
thread_local! {
    static LOGIN_SHELL_ASKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// [`login_shell_path`]'s body over an explicit program, argv and budget, so the
/// budget is testable against a plain `/bin/sh`.
#[cfg(any(not(windows), test))]
fn login_shell_path_with(
    program: &str,
    args: &[&str],
    budget: std::time::Duration,
) -> Option<String> {
    #[cfg(test)]
    LOGIN_SHELL_ASKS.with(|asks| asks.set(asks.get() + 1));
    // Below the typing band: this runs the user's whole rc chain for an atpkg
    // pass, and a thread's QoS does not cross the spawn (`qos::command`).
    let mut command = crate::qos::command(crate::qos::Role::Background, program);
    command
        .args(args)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .stderr(aterm_uds::spawnfd::Stdio::null());
    // Its own session, so a timeout kills what the rc file is blocked in (a
    // `sleep`, a password helper) along with the shell: those grandchildren hold
    // the stdout pipe too, and a session leader's pid is also its group's id, so
    // `killpg(child.id())` below reaches them. A session, not just a group: when
    // this GUI sits in the foreground of a controlling terminal (`aterm --window`
    // typed at a shell, `targo run`), `zsh -i` in a background group opens
    // `/dev/tty`, takes SIGTTOU and sits stopped until the budget runs out; a new
    // session has no controlling tty. (A spawn attribute: `POSIX_SPAWN_SETSID` on
    // Darwin, no code of ours between a fork and an exec.)
    #[cfg(unix)]
    command.setsid(true);
    let mut child = command.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    // Drain on a helper so a chatty rc file cannot fill the pipe. The helper
    // streams what it reads and is never joined: an rc file's background job can
    // hold the pipe's write end for as long as it lives.
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
    // THE ANSWER AT THE SHELL'S EXIT, not at the next tick of a 25 ms nap (measured:
    // an empty-rc login zsh answers in under 10 ms and was read at 25-30): the kernel's
    // exit witness, which reaps nothing, so the shell's pid still names its group for
    // the kill below. Without one (the shell already gone, or no watch on this
    // platform), and once it fires for a shell that has not exited or `poll` itself
    // fails (never a spin on an error), the nap.
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
                // The whole group (the session leader's pid is its group id),
                // before the leader is reaped (its pid names the group until then).
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
    // The shell exited 0, so its PATH line is in the pipe. Collect to EOF, but no
    // further than the budget plus a grace, and once a PATH line is in hand only
    // until the pipe is quiet for that grace: an rc file's background job with
    // inherited stdout keeps the pipe open and is left running.
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

/// The PATH line out of a login shell's output: the last non-empty line that starts
/// with `/` (rc-file banners print before it; a PATH may contain spaces — the managed
/// store lives under `Application Support`). Pure, so the shape is testable.
#[cfg(any(not(windows), test))]
pub(crate) fn pick_login_path(output: &str) -> Option<String> {
    output
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| line.starts_with('/') && line.contains(':'))
        .map(str::to_string)
}

/// The fallback child PATH: the process PATH (launchd's four dirs, for a Finder
/// launch) plus the places a foreign `claude`/`codex` measurably lives —
/// `~/.local/bin` (Anthropic's native installer), on macOS `/opt/homebrew/bin`
/// (casks), and on Unix `/usr/local/bin` — each appended once. Pure.
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
    // The shell hook's two placements (`atpkg::hooks`): `agents/` moved to the
    // front, `bin/` appended once.
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

    /// The PATH a tab's shell is handed: the reroute dir first, the agents dir
    /// beside it, then the bundle dir, then the inherited PATH (`bin/` behind it:
    /// `the_bin_dir_is_appended_once_behind_the_inherited_path`). Each injected
    /// dir is MOVED to its place, never duplicated (a nested aterm inherits a
    /// PATH listing the reroute dir behind `~/.cargo/bin`); the bundle dir is
    /// never stacked when already reachable; an empty entry ("here") is the
    /// user's and survives; nothing to inject is no pair at all; and
    /// reapplying the seam to its own answer changes nothing.
    #[test]
    fn the_injected_dirs_lead_the_inherited_path_once_each() {
        /// What it is, `[reroute, agents, bundle]`, the inherited PATH, the PATH handed.
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

    /// The spawn seam creates the agents dir rather than waiting for it: a fresh
    /// prefix has no `agents/`; after this call it exists (private) and is the
    /// string the seam front-inserts. A second call is a no-op.
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
        // A symlink at `agents/`, even one to a real directory, and a regular file
        // there are refused (`Path::is_dir` would follow the link).
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

    /// The atpkg children's PATH: the login shell's answer is the last `/…:…` line
    /// (banners before it, spaces inside it); the fallback appends the two foreign
    /// homes and `/usr/local/bin` to whatever the process had, once each.
    ///
    /// Unix-only: launchd's PATH, a login shell's `:`-joined answer and
    /// `/Users//u` are the macOS fixtures, and `Path::join` on Windows would
    /// spell the home's `.local/bin` with a backslash.
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
        // Homebrew's prefix is a macOS place: nowhere else does the fallback name it.
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
        // The managed dirs ride as the shell hook places them: agents/ first
        // (moved, never duplicated), bin/ last once.
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

    /// The shell the atpkg children's `PATH` is read from: the account's when its
    /// `-l -i -c` reads the login files and speaks POSIX `printf`, else
    /// `/bin/zsh` on macOS and `/bin/sh` elsewhere — never a `/bin/zsh` a Linux
    /// box does not have, and never tcsh or nu.
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

    /// PKG-6 #9 — A FIRST RUN'S UPDATE NEVER WAITS ON THE PERSON'S RC CHAIN. With the login
    /// shell still out (a heavy rc), a first run takes the fallback at once and never
    /// waits; a routine pass waits for the shell's answer; once the shell has answered,
    /// both take it.
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

    /// NO LOGIN SHELL IS ASKED ON WINDOWS (2026-09-30). The window has no
    /// console, so a console `bash` it starts puts a console window on screen
    /// for its whole rc chain, and the `PATH` a Git Bash prints
    /// (`/c/Users/…:/usr/bin`) is no Windows search path anyway. `SHELL` names a
    /// bash, as Git Bash and MSYS2 export it.
    ///
    /// The proof is a count, not the `None` alone: a missing bash also answers
    /// `None`, by a failed spawn, so `login_shell_path` must not enter
    /// [`super::login_shell_path_with`] at all (`LOGIN_SHELL_ASKS`). The control
    /// shows a direct ask is counted and, where Git for Windows is installed,
    /// answers a `PATH`.
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

    /// The rc files run below the typing band: a thread's QoS does not cross the
    /// spawn, so the lookup clamps the shell itself (`qos::command`). The probe
    /// shell reports its own priority as its "PATH"; the verdict is that band, and
    /// the budget only ends a lookup that never answers.
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

    /// The budget bounds the wait, not just the shell: an rc file blocked in an
    /// external command (`sleep`) leaves that grandchild holding the stdout pipe
    /// after the shell is killed, and the lookup must not wait for it.
    ///
    /// The bound is the grandchild's own lifetime: a lookup that waits for EOF
    /// cannot return before the `sleep` ends, which is after `JOB`.
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

    /// ...and an rc file that backgrounds a job without redirecting its stdout
    /// (`somedaemon &`) must not hold the answer once the shell has printed its
    /// PATH and exited.
    ///
    /// The bound is the job's own lifetime: a lookup that waits for EOF on the
    /// pipe the job holds returns after `JOB`, so one that returns before did not
    /// wait. The budget is a hang detector (the shell runs behind the utility
    /// clamp, `qos::command`). The answer carries the job's pid so the test can
    /// end it: a job left running would keep every unflagged fd this process had
    /// open at the fork.
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
        // Before `JOB` the job is still sleeping, so its pid is still its own.
        if let Some(pid) = job.filter(|_| elapsed < JOB) {
            // SAFETY: `kill` takes no pointers, and `pid` is positive, so it
            // names one process and never a group.
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
        assert!(
            elapsed < JOB,
            "waited for the backgrounded job: {elapsed:?}"
        );
        assert!(job.is_some(), "no PATH carrying the job's pid: {path:?}");
    }

    /// A GUI with a foreground controlling terminal (`aterm --window` typed at a
    /// shell, `targo run`) still gets its answer: a lookup shell in a background
    /// group would take SIGTTOU on `/dev/tty` and stop.
    ///
    /// Each interactive shell the host has answers: zsh (`-f`, no rc file), the
    /// macOS default where the stop was found, and bash (`--norc --noprofile`),
    /// the lookup's shell for Linux's default account, which stops the same way
    /// (it signals its own group SIGTTIN until it holds the foreground). A host
    /// with neither cannot run it.
    ///
    /// The lookup runs in a re-exec of this test binary (a stand-in, judged by
    /// its own `1 passed`) that leads a session on a pty of its own, so the tty
    /// stays out of the parallel test process. The answer is the check, not a
    /// stopwatch: a stopped shell never exits, so the regression fails the `Some`
    /// assertion at any budget. The budget is only the backstop, generous (the
    /// shell runs behind the utility clamp, `qos::command`) and inside the
    /// stand-in's hang detector so the child reports its own `None`.
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
            // This process must be in the foreground of a controlling tty, or the
            // shell has nothing to stop on and a pass would prove nothing.
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
        // The re-exec is this test as a stand-in on the slave of a fresh pty:
        // session leader of its own session, the pty its controlling terminal,
        // so its process group holds the foreground (what `login_tty` does in
        // `forkpty`'s child). Both ends are close-on-exec from birth: a
        // sibling test's child forked meanwhile never holds the master past
        // this test's close.
        let (master, slave) = aterm_pty::open_pty_pair().expect("a pty pair");
        // SAFETY: two live descriptors this test owns, each taken by exactly
        // one `OwnedFd`; the stand-in's stdin is the copy std makes of `slave`.
        let (master, slave) = unsafe {
            use std::os::fd::{FromRawFd as _, OwnedFd};
            (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave))
        };
        let mut stand_in = crate::test_alone::StandIn::this_test();
        // An interactive shell with no rc file must never read or trim the
        // person's history file at its default size: it gets none.
        stand_in
            .command()
            .env(CHILD, "1")
            .env("HISTFILE", "/dev/null")
            .stdin(std::process::Stdio::from(slave));
        // A controlling terminal is taken only in the child, so this stand-in
        // alone takes std's fork path (a hook) rather than `posix_spawn`.
        // SAFETY: the hook runs between fork and exec and calls only `setsid`
        // and `ioctl`, both async-signal-safe, on the child's own stdin.
        unsafe {
            std::os::unix::process::CommandExt::pre_exec(stand_in.command(), || {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        // The master stays open until the stand-in has ended: closing it
        // first would hang its terminal up under it.
        stand_in.spawn().passed();
        drop(master);
    }

    /// `bin/` goes LAST, once: appended behind every inherited entry (a system copy of
    /// a name wins), never stacked on a PATH that already holds it — the hook sourced
    /// later skips it by the same test — while reroute and agents stay in front. It is
    /// an injection on its own, and `None` leaves every other arm as it was.
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

    /// Inside a .app bundle the executable's own directory is the bundle dir; inert
    /// outside one (dev builds) and safe with no exe at all. On Linux, which has no
    /// bundle, it is the store folder install.sh lays, judged on disk here: nothing
    /// until the `aterm-ctl` alias stands beside the binary, then the folder.
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

    /// The Linux store rule ([`super::linux_store_dir`]): the running `aterm`'s folder when
    /// its `aterm-ctl` sibling is an alias of it, which is how install.sh lays the store.
    /// No sibling, a build tree's `aterm-ctl` (a dev binary that resolves to itself), or a
    /// binary under another name puts nothing on PATH. The folder then leads the inherited
    /// PATH behind the reroute and agents dirs, as the macOS bundle does.
    #[test]
    fn linux_store_dir_requires_aterm_ctl_sibling() {
        use super::linux_store_dir;
        use std::path::PathBuf;
        const STORE: &str = "/home/u/.local/lib/aterm/bin";
        let exe = Path::new(STORE).join("aterm");
        let ctl = Path::new(STORE).join("aterm-ctl");
        let installed = |path: &Path| (path == exe || path == ctl).then(|| exe.clone());
        assert_eq!(linux_store_dir(&exe, installed).as_deref(), Some(STORE));
        // NEGATIVE CONTROL: the same binary with no aterm-ctl beside it.
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

    /// On Windows the app folder is the running image's folder when the console
    /// image `aterm.exe` sits in it, whichever image runs the window; a folder
    /// without it, or no exe at all, is nothing. The folder then leads the
    /// inherited PATH behind the reroute and agents dirs, once.
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
        // NEGATIVE CONTROL: no console image beside the window's image.
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

    /// Where an OSC 52 Set writes and its query reads, per target: `'c'` and
    /// the empty xterm default are the clipboard; `"cp"` sets both and keeps
    /// the established clipboard priority for its one response; `'p'` is
    /// PRIMARY on X11, where a Set and its query meet in the same slot. Off
    /// X11 there IS no PRIMARY: `'p'` folds into the clipboard on both sides
    /// of the round trip (what every macOS/Windows OSC 52 client relies on),
    /// and `"cp"` collapses to ONE clipboard write, not two.
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

    /// End-to-end on the stubbed slots (Linux): with different text in the
    /// clipboard and PRIMARY own-slots, the routed query reader returns the
    /// PRIMARY text for 'p' and the clipboard text for 'c'.
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

    /// The receiver drains the retained final value before observing closure.
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

    /// Tier-1: drive the real constant-memory mailbox through the derived model's
    /// action trace and project its one physical slot onto the model after every
    /// transition. The mutant is an unbounded/stale queue, so the check is not
    /// vacuous.
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

/// What aterm does for a session's program by the containment mode it was
/// spawned under ([`super::host_reach`]): its OSC 52 and its desktop
/// notifications here, its Kitty file mediums in `kitty_transfer_tests`.
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

    /// A wait for a write that must arrive: a hang detector, never a budget.
    const HANG: Duration = Duration::from_secs(60);

    /// OSC 52 sets the clipboard to `planted`.
    const SET: &[u8] = b"\x1b]52;c;cGxhbnRlZA==\x07";

    /// The configuration that lets OSC 52 read the clipboard back.
    fn query_allowed() -> TerminalConfig {
        TerminalConfig {
            allow_osc52_query: true,
            ..TerminalConfig::default()
        }
    }

    /// A live session built as `spawn_session` builds one under `mode` (the
    /// engine from its config, then the clipboard, then the standard policy),
    /// with the platform write replaced by a channel the test reads.
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

    /// The reply to an OSC 52 query, fed as the PTY reader feeds it, while the
    /// clipboard (the test build's board, read on this thread) holds
    /// `owner-secret`.
    fn query_reply(term: &Arc<Mutex<Terminal>>) -> Option<Vec<u8>> {
        crate::control::PBPASTE_STUB.with(|b| *b.borrow_mut() = Some("owner-secret".into()));
        crate::host_io_prefetch::feed_as_reader(term, b"\x1b]52;c;?\x07");
        let reply = term_lock(term).take_response();
        crate::control::PBPASTE_STUB.with(|b| *b.borrow_mut() = None);
        reply
    }

    /// An OSC 52 query's pasteboard read is never made inside `process()`, under
    /// the terminal lock: a read that blocks (a Universal Clipboard fetch, a busy
    /// provider) would hold the lock every window's redraw takes. The reader
    /// reads before its hold and the query is answered from that read, in its
    /// place among the burst's replies; fed by a hold alone it is unanswered.
    /// (Linux answers from aterm's own selection, which never blocks, in the hold.)
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
        // A DA1 asked right after it is answered after it, as the program asked.
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

    /// A session's clipboard worker starts with its first OSC 52 write, never
    /// with the session: a tab that never writes the clipboard holds no parked
    /// thread, one more stack in every later fork's VM map. The first write
    /// still arrives, and a second starts no second worker. NEGATIVE CONTROL:
    /// before any write, no worker started.
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

    /// A program in a Containment tab neither sets nor reads the clipboard
    /// through OSC 52, even after a later grant of the query (a config
    /// reload); a program in any other mode's tab still does both, which is
    /// what makes the refusal mean something.
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
        // Nothing to deliver to: the refused session dropped the writer unused.
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
        // A reload re-applies `allow_osc52_query = true` to every live engine.
        term_lock(&term).apply_config(&query_allowed());
        assert_eq!(
            query_reply(&term),
            None,
            "a contained query is answered after a reload"
        );
    }

    /// The configuration that admits a program's desktop notifications.
    fn notifications_allowed() -> TerminalConfig {
        TerminalConfig {
            allow_notifications: true,
            ..TerminalConfig::default()
        }
    }

    /// A notification in each of the three spellings: OSC 9, OSC 99 (kitty)
    /// and OSC 777 (rxvt-unicode).
    const NOTICES: [&[u8]; 3] = [
        b"\x1b]9;from the tab\x07",
        b"\x1b]99;;from the tab\x1b\\",
        b"\x1b]777;notify;title;from the tab\x07",
    ];

    /// How many notifications `term`'s program posts by printing [`NOTICES`].
    /// The callbacks run inside `process` and only `try_send`, so whatever
    /// was posted is queued when it returns: no wait decides it.
    fn posted(term: &Arc<Mutex<Terminal>>, notices: &Receiver<super::notify::NotifyMsg>) -> usize {
        for notice in NOTICES {
            term_lock(term).process(notice);
        }
        notices.try_iter().count()
    }

    /// A program in a Containment tab posts no desktop notification, even
    /// with `allow_notifications` on and after a reload re-grants it; a
    /// program in any other mode's tab posts all three spellings, which is
    /// what makes the refusal mean something.
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

    /// What a `bytes` subscriber's queue holds at the instant the wake fires.
    fn queued_at_wake(publish: impl FnOnce(&ByteFanout, &Arc<[u8]>, &dyn Fn())) -> usize {
        let fanout = Arc::new(ByteFanout::new());
        let sub = fanout.subscribe();
        let seen = std::cell::Cell::new(None);
        let burst: Arc<[u8]> = Arc::from(&b"out"[..]);
        publish(&fanout, &burst, &|| seen.set(Some(sub.drain().0.len())));
        seen.get().expect("the wake fired")
    }

    /// A WOKEN `bytes` SUBSCRIBER FINDS THE BURST: the reader's wake fires
    /// after the tee. NEGATIVE CONTROL: waking first, then teeing, leaves the
    /// woken subscriber an empty queue, and the burst waits for a later wake.
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

    /// A BURST CARRIES ITS ARRIVAL ON THE FRAME CLOCK: the reader's reading
    /// reaches the subscriber's queue as that instant's `now_us` stamp, which
    /// `instant_at_us` places back at it to the microsecond, so a stamped
    /// `BYTES` frame reads on `video`'s clock.
    #[test]
    fn a_burst_carries_its_arrival_on_the_frame_clock() {
        let fanout = Arc::new(ByteFanout::new());
        let sub = fanout.subscribe();
        // The clock's anchor predates the reading, as it does from startup.
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
    //! The reader's held gather (`begin_held_gather`, `gather_held_output`): a
    //! line-per-call writer's reads are gathered while the UI thread holds the
    //! session's latch, and fed once per hold instead of once per line.
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

    /// A gather begins only on a held latch, below a batch's worth, with no
    /// key waiting; it marks the hold dirty as it begins, and ends a margin
    /// before the hold is due, never sooner than the shortest gather and never
    /// later than a cadence.
    #[test]
    fn a_gather_begins_only_on_a_held_latch_and_ends_before_its_due() {
        let now = 100 * MS;
        // Clear, armed and posted latches: the wake protocol's, fed at once.
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
        // A key waiting for its echo, or a read already a batch: fed at once.
        for (filled, typing) in [(100, true), (HELD_GATHER_ENOUGH, false)] {
            let latch = AtomicU64::new(held);
            assert_eq!(begin_held_gather(&latch, now, filled, typing), None);
            assert_eq!(latch.load(Ordering::Relaxed), held, "untouched");
        }
        // Held: gathered until a margin before the due instant, the hold marked
        // dirty so its release runs the pass whenever the gather is fed.
        let latch = AtomicU64::new(held);
        assert_eq!(
            begin_held_gather(&latch, now, 100, false),
            Some(due - HELD_GATHER_MARGIN_NS)
        );
        assert_eq!(latch.load(Ordering::Relaxed), held | WAKE_LATCH_DIRTY);
        // Already dirty: the same answer, the word unchanged.
        assert_eq!(
            begin_held_gather(&latch, now, 100, false),
            Some(due - HELD_GATHER_MARGIN_NS)
        );
        assert_eq!(latch.load(Ordering::Relaxed), held | WAKE_LATCH_DIRTY);
        // Near or past due (a late release): the shortest gather.
        for at in [due - HELD_GATHER_MARGIN_NS, due, due + 7 * MS] {
            let latch = AtomicU64::new(held);
            assert_eq!(
                begin_held_gather(&latch, at, 100, false),
                Some(at + HELD_GATHER_MIN_NS)
            );
        }
        // Never longer than a cadence.
        let far = AtomicU64::new(WAKE_LATCH_HELD | (now + 50 * MS));
        assert_eq!(
            begin_held_gather(&far, now, 100, false),
            Some(now + OUTPUT_CADENCE_NS)
        );
    }

    /// A pipe a writer fills one line every `gap` from `first` on, read with a
    /// clock the gather's waits move: what [`gather_held_output`] collects.
    struct Pipe {
        first: u64,
        gap: u64,
        len: usize,
        lines: usize,
        taken: Cell<usize>,
        clock: Cell<u64>,
    }

    impl Pipe {
        /// Every line written by the clock and not yet read, into `buf[filled..]`
        /// as far as it reaches.
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

    /// A gather collects each line the pipe holds by its end, waiting in steps
    /// no longer than [`HELD_GATHER_STEP_NS`] and asking short of its deadline
    /// by the wait's overshoot, so it ends on time; a key arriving ends it at
    /// once; a fast writer ends it at a batch's worth.
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
        // Each wait overshoots as Windows' does (`HELD_GATHER_WAIT_SLACK_NS`).
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
        // A key arriving mid-gather ends it at once: the echo is fed now.
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
        // A writer faster than the gather fills a batch's worth at the first look.
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

    /// The UI thread's side, as `output_wake_coalesce_tests::drive` models it:
    /// a posted wake's pass 20 µs after the post, and each hold's release
    /// `late` after its due instant (a dirty one runs the pass and holds again).
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

        /// Every turn due at or before `t`, in order.
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

    /// How a driven reader's machine keeps time: each feed costs `feed`, each
    /// gather wait wakes `overshoot` past its ask, and each hold is released
    /// `late` after it falls due.
    #[derive(Clone, Copy, Debug)]
    struct Timing {
        feed: u64,
        overshoot: u64,
        late: u64,
    }

    /// Drive a reader over `pipe` through the real gate, pass, release and (when
    /// `gather`) held gather: a blocking read takes every line written by the
    /// clock, and the UI turns as [`Ui`] says, all on `timing`'s clocks.
    /// Returns, per line, when it was fed, the feeds, and the UI's passes.
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
            // The blocking read returns once the next line is written.
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
            // Fed: the engine has the lines, then the gate runs.
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

    /// The finding (re-judge, 2026-10-18): after the holds, 200,000 lines of
    /// pwsh `Get-Content` still cost aterm 7.5-8.9 s of CPU against 0.3 s for
    /// `cat`, because the reader still fed, locked, taped and gated each line on
    /// its own. 200 ms of lines 100 µs apart now cost a feed per line only for
    /// the first frame and then at most four per hold, and every line still
    /// reaches a pass within a cadence plus the gather's margin and its
    /// shortest wait (plus the release's own lateness).
    ///
    /// On exact clocks, and on Windows-like ones: waits that overshoot by
    /// [`HELD_GATHER_WAIT_SLACK_NS`] and releases up to a millisecond late.
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
            // At most four feeds per hold: the long gather, and the short ones
            // around the release (measured on these clocks: 253 to 344 feeds).
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
            // Without the gather, the same writer is fed line by line.
            let (_, feeds_ungathered, _) = drive_reader(&pipe(), false, timing);
            assert_eq!(feeds_ungathered, 2_000, "{timing:?}");
        }
    }

    /// A typed echo meets no hold, so nothing is gathered: under OpenConsole a
    /// PSReadLine echo is about seven reads 40 µs apart, each fed as it comes.
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

    /// A burst of posts, each under a cadence after the one before, streams
    /// once it has run [`STREAM_AFTER_NS`] (posted too sparsely to be dense,
    /// [`STREAM_AFTER_POSTS`]). A quiet cadence ends a burst that has
    /// not streamed yet; one that has outlives it, and its held chunks carry it,
    /// until a quiet frame ends it.
    #[test]
    fn a_burst_streams_after_a_frame_and_a_quiet_frame_ends_it() {
        let mut c = PostCadence::default();
        assert!(!c.streaming(5 * MS), "a reader's first post");
        let mut t = 5 * MS;
        c.posted(t);
        // Ended by a quiet cadence before it streamed.
        assert!(!c.streaming(t + OUTPUT_CADENCE_NS));
        t += OUTPUT_CADENCE_NS;
        c.posted(t);
        let from = t;
        // 2 ms apart: fewer posts in a frame than make a burst dense.
        while t + 2 * MS < from + STREAM_AFTER_NS {
            t += 2 * MS;
            assert!(!c.streaming(t), "+{} µs into the burst", (t - from) / US);
            c.posted(t);
        }
        t += 2 * MS;
        assert!(c.streaming(t), "a frame into the burst");
        c.posted(t);
        // Under its hold the stream feeds without posting.
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

    /// A post that is not streaming clears the latch at its pass; a streaming
    /// one carries the streaming bit, and its pass holds the latch for the
    /// cadence instead.
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
        // A pass that runs on a hold (its release, or a redraw nudge meanwhile)
        // holds again from its own instant.
        assert_eq!(
            output_latch_after_pass(held | WAKE_LATCH_DIRTY, 30 * MS),
            WAKE_LATCH_HELD | (30 * MS + OUTPUT_CADENCE_NS)
        );
    }

    /// A held latch posts nothing: the first chunk marks it dirty, the next ones
    /// find it so. Released while clean, it clears and the next chunk posts at
    /// once; due and dirty, the release asks for the pass.
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
        // The pass the release asks for holds again.
        let word = latch.load(Ordering::Relaxed);
        latch.store(output_latch_after_pass(word, due), Ordering::Relaxed);
        let due2 = due + OUTPUT_CADENCE_NS;
        assert_eq!(latch.load(Ordering::Relaxed), WAKE_LATCH_HELD | due2);
        // Nothing fed under the second hold: released clean.
        assert_eq!(release_output_hold(&latch, due2), HoldRelease::Released);
        assert_eq!(latch.load(Ordering::Relaxed), 0);
        assert_eq!(release_output_hold(&latch, due2), HoldRelease::NotHeld);
        // The next chunk posts at once.
        assert_eq!(
            gated_output_wake(&latch, due2 + MS, 0, false, || true, || due2 + MS),
            WakeGate::Posted
        );
        assert_eq!(release_output_hold(&latch, u64::MAX), HoldRelease::NotHeld);
    }

    /// The lost-wake watchdog and the stranded re-post leave a hold alone: no
    /// wake stands behind it, and its release has its own deadline.
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
        // A streaming wake that IS lost is still the watchdog's to heal.
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

    /// Drive the real gate, pass and release: a reader feeds a chunk at each of
    /// `reads` (ascending ns), and the UI thread turns 20 µs after each post (the
    /// pass, then `about_to_wait`'s release) and at each hold deadline it folded.
    /// Returns the instants of the passes, how many hold deadlines were folded,
    /// and the latch word once the loop has nothing left to do.
    fn drive(reads: &[u64]) -> (Vec<u64>, usize, u64) {
        drive_with_edges(reads, &[])
    }

    /// [`drive`], where the chunk fed at each instant in `edges` carries a
    /// foreground edge (`ingest`'s [`PostCadence::foreground_edge`] before it).
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
            // Every turn due by this read runs before it.
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

    /// Every chunk is read by a pass at most `bound` after it was fed.
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

    /// The finding (judge, 2026-10-18): a writer that hands its reader one line
    /// every 100 µs bought one loop turn per read, 19,133 `Wake::Output`s for
    /// 20,000 lines of pwsh `Get-Content`. Through the real gate, pass and
    /// release, 200 ms of such output now costs a pass per read for its first
    /// frame and one per cadence after it, and every line, the last included,
    /// still reaches a pass within one cadence.
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

    /// The finding (CTL-1, 2026-10-19): on a busy Mac the writer of a flood is
    /// descheduled for 4 ms and more, dozens of times a second, and each such
    /// stall ended its hold chain with a clean release; the stream then bought a
    /// pass per read for a frame again. 4,000 lines 100 µs apart, the writer
    /// stalled 6 ms after every 200 of them, now cost one pass per read for the
    /// first frame, then one per cadence and one at each stall's end (276
    /// passes), and every line still reaches a pass within a cadence. NEGATIVE
    /// CONTROL: a burst ended at each stall (the old `PostCadence`) makes 3,240
    /// passes, against the bound of 332 here.
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

    /// A burst streams at its [`STREAM_AFTER_POSTS`]th post short of a frame, and
    /// is dense from there: a quiet spell under a frame no longer ends it. A
    /// burst of fewer posts (a typed echo's seven) does not stream, and a quiet
    /// cadence ends it.
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

    /// The finding (WIN-1, 2026-10-20): ten tabs flooding behind a window's idle
    /// front tab, under load, restarted their bursts at every 4 ms stall of the
    /// writer before a burst had run a frame (2,350 restarts in 8.4 s), and each
    /// restart bought a pass per read for its frame: 374,000 `Wake::Output`s and
    /// 278 ms/s of UI-thread work. Through the real gate, pass and release: reads
    /// 100 µs apart whose writer stalls 5 ms after every 8 ms of them never run a
    /// frame unbroken, yet now cost at most the dense count's passes, then one per
    /// cadence and one at each stall, and every line still reaches a pass within a
    /// cadence. NEGATIVE CONTROL: without the dense count (the old `PostCadence`)
    /// every read is a pass, 4,000 against the bound of 281.
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

    /// A prompt after a program's output is never held, however that output
    /// ran: the prompt's three reads 200 µs apart (the holder's change, the
    /// shell's marks, the prompt), the first carrying the program's foreground
    /// edge, each get their pass at once, after a 40 ms flood (streamed and
    /// held, the prompt 1 ms into its last hold), after 40 lines 300 µs apart
    /// (dense, the prompt 500 µs after them), and after the same lines with a
    /// 6 ms stall before the prompt (a busy Mac). NEGATIVE CONTROL: with
    /// `foreground_edge` a no-op, the prompt waits for a release in all three.
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

    /// A key's echo is never held. Under OpenConsole a PSReadLine echo is about
    /// seven reads 40 µs apart (cursor off, text, colours, cursor on; measured
    /// in a typing cast, 2026-10-18): each still buys its pass at once, and no
    /// hold deadline is ever folded.
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

    /// Through the real `App`: a streaming wake's pass holds session 0's latch,
    /// and `release_output_holds` folds the hold's due instant, runs the
    /// session's pass at it for a chunk fed under the hold (its window admitted
    /// again), holds again, and clears once nothing more was fed.
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
        // A streaming wake, posted and delivered.
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
        // A chunk fed under the hold posts nothing.
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
        // Nothing more was fed: released clean, with no pass and no deadline.
        app.windows
            .get_mut(&wid)
            .expect("window 0")
            .pending_deco_birth = None;
        let at_due2 = at_due + Duration::from_nanos(OUTPUT_CADENCE_NS);
        assert_eq!(app.release_output_holds(at_due2), None);
        assert_eq!(latch.load(Ordering::Relaxed), 0);
        assert_eq!(app.windows[&wid].pending_deco_birth, None);
        // A lone wake on the cleared latch clears it again: no hold, no deadline.
        latch.store(ns(at_due2) | WAKE_LATCH_SENT, Ordering::Relaxed);
        app.on_output_wake(0, at_due2);
        assert_eq!(latch.load(Ordering::Relaxed), 0);
        assert_eq!(app.release_output_holds(at_due2), None);
    }

    /// A failed send (headless: no event loop) resets the latch: an arm left
    /// behind would suppress wakes until the watchdog proved it lost.
    #[test]
    fn failed_send_resets_the_flag() {
        let flag = AtomicU64::new(0);
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        drop(rx); // no event loop: every send errors
        let gate = gated_output_wake(&flag, 1, 0, false, || tx.send(()).is_ok(), || 2);
        assert_eq!(gate, WakeGate::Coalesced, "nothing was posted");
        assert_eq!(
            flag.load(Ordering::Relaxed),
            0,
            "failed send must not leave the latch armed"
        );
        // A later live receiver is still woken.
        let (tx2, rx2) = std::sync::mpsc::channel::<()>();
        let gate = gated_output_wake(&flag, 3, 0, false, || tx2.send(()).is_ok(), || 4);
        assert_eq!(gate, WakeGate::Posted);
        assert_eq!(rx2.try_iter().count(), 1);
    }

    /// The arm is two-phase: while `send` runs the word is the bare arm stamp
    /// (unsent), and after it returns the post-send stamp with
    /// [`WAKE_LATCH_SENT`]. (That an armed latch then coalesces however old is
    /// `latch_conforms_to_damage_to_present_model`'s.)
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

    /// A handler that consumes the event before the reader re-stamps has
    /// already cleared the latch; the re-stamp must not re-arm it.
    #[test]
    fn a_handler_clear_during_the_post_stands() {
        let flag = AtomicU64::new(0);
        gated_output_wake(
            &flag,
            5,
            0,
            false,
            || {
                flag.store(0, Ordering::Relaxed); // the handler ran already
                true
            },
            || 6,
        );
        assert_eq!(flag.load(Ordering::Relaxed), 0);
    }

    /// Model states reachable at the committed `Buggy = 0`.
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

    /// Time scaling: `K` ns per model tick, bracketed so the model's `Expiry`
    /// ticks and the code's [`WAKE_LATCH_EXPIRY_NS`] agree exactly on both
    /// sides of the staleness boundary: `(n-l)*K ± 1 > EXPIRY_NS <=> n-l > Expiry`.
    const K: u64 = 30_000_000;
    const EXPIRY_TICKS: u64 = 3; // the model's `Expiry`

    /// The latch word the model's `(latch, seen)` stands for: the model posts
    /// atomically with the arm, so every armed latch is sent. Its stamp sits just
    /// before the tick when a `TurnEnd` saw it (the post preceded that pass, whose
    /// stamp is `drained`), and just after it otherwise: the model's tick ties
    /// resolved as the real clocks order them.
    fn word(st: &aterm_spec::interp::State) -> u64 {
        let latch = st["latch"] as u64;
        match (latch, st["seen"]) {
            (0, _) => 0,
            (l, 1) => (l * K - 1) | WAKE_LATCH_SENT,
            (l, _) => (l * K + 1) | WAKE_LATCH_SENT,
        }
    }

    /// What the readers see of the loop ([`LoopIdle`]): `0` while a turn runs,
    /// else the last `TurnEnd`'s pass stamp.
    fn idle_since(st: &aterm_spec::interp::State) -> u64 {
        if st["busy"] == 1 {
            0
        } else {
            st["drained"] as u64 * K
        }
    }

    /// Tier-1 conformance, reader half: at every state of
    /// `aterm_models_core::damage_to_present_model()` reachable under `Buggy = 0`,
    /// the real [`gated_output_wake`], fed the projected latch word and loop idle
    /// stamp, posts iff `DamageFresh` is enabled, re-posts iff `DamageRepost` is,
    /// re-stamps the latch to the model successor's word, and otherwise coalesces.
    ///
    /// Negative controls: a reader that re-sends on an arm older than the expiry
    /// disagrees with the model on a wake queued behind a long frame; a reader
    /// that never re-sends disagrees on a wake stranded with its loop wake-up.
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
            // Negative control 1: the age re-send (send iff clear or stale).
            let at = before & !WAKE_LATCH_SENT;
            let age_sends = before == 0 || now_ns.saturating_sub(at) > WAKE_LATCH_EXPIRY_NS;
            if age_sends && model_posts.is_none() && st["inflight"] == 1 {
                duplicate_caught = true;
            }
            // Negative control 2: never re-send on an armed latch; it swallows a
            // stranded wake the model re-posts.
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

    /// Tier-1 conformance, watchdog half: at every reachable state where the
    /// model's `about_to_wait` (`TurnEnd`) can run, the real [`judge_output_wake`]
    /// returns `Lost` iff `TurnEnd` heals, and its recheck is exactly the deadline
    /// `TurnEnd` folds (none when it heals or the latch is clear). A deadline
    /// exists only when a `TurnEnd` observed the arm, iff the real idle stamp is
    /// newer than the latch stamp.
    ///
    /// Negative controls: an age-only verdict heals on a reachable state where the
    /// wake is still in flight; a `stamp + expiry` recheck folds a past deadline
    /// where the model folds a future one.
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
                // The age rule diverges both ways: it heals a proven-lost young
                // wake later, and it heals a queued wake (the false heal).
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

    /// End to end on the real latch and watchdog: a wake posted at the start of
    /// a 300 ms frame is still queued when the frame ends. The watchdog books it
    /// late (no heal), the reader posts no duplicate however many chunks arrive,
    /// and the next turn's drain delivers it.
    #[test]
    fn a_300ms_frame_with_an_in_flight_wake_counts_no_heal() {
        let latch = AtomicU64::new(0);
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let mut watch = OutputWakeWatch::default();
        let loop_idle = LoopIdle::default();
        let t0 = 1_000 * MS;
        // Turn k-1's `about_to_wait`.
        assert_eq!(watch.pass(t0, [(7, &latch)]).healed, 0);
        loop_idle.went_idle(t0);
        // Turn k: the drain finds nothing; the frame starts. Output lands 10 ms in.
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
        // The frame ends 300 ms later: turn k's `about_to_wait`.
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
        // Armed and older than the expiry, yet not lost.
        assert!(end - (t0 + 10 * MS) > WAKE_LATCH_EXPIRY_NS);
        loop_idle.went_idle(end);
        // A chunk between this `about_to_wait` and the next turn: the pass saw
        // the wake, so it is not stranded.
        let gate = gated_output_wake(
            &latch,
            end + MS,
            loop_idle.since_ns(),
            false,
            || tx.send(()).is_ok(),
            || end + MS,
        );
        assert_eq!(gate, WakeGate::Coalesced);
        // Turn k+1 (the queued wake turns the loop at once): the drain delivers
        // the one wake and the handler clears.
        assert_eq!(rx.try_iter().count(), 1, "no duplicate wake was posted");
        latch.store(0, Ordering::Relaxed);
        let next = watch.pass(end + 2 * MS, [(7, &latch)]);
        assert_eq!((next.healed, next.late, next.recheck_ns), (0, 0, None));
    }

    /// Through the real scheduler, under 400 ms frames with a wake posted 5 ms
    /// into each: a late wake's recheck must never be in the past, or the
    /// deadline healer's stale-arm backstop fires on every turn. A late wake is
    /// queued and turns the loop by itself.
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
            // The turn's drain delivers the queued wake; the handler clears.
            latch.store(0, Ordering::Relaxed);
            // 5 ms in, output posts a wake; the frame runs 400 ms.
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
            // The fold reads its clock after the pass.
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

    /// A wake that is lost after a pass saw it is still healed within its
    /// bound: the drain after its post ran and nothing cleared the latch.
    #[test]
    fn a_truly_lost_wake_still_heals_within_its_bound() {
        let latch = AtomicU64::new(0);
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let mut watch = OutputWakeWatch::default();
        let t0 = 1_000 * MS;
        watch.pass(t0, [(3, &latch)]);
        let sent = t0 + 10 * MS;
        gated_output_wake(&latch, sent, 0, false, || tx.send(()).is_ok(), || sent);
        // Turn k drains the event but it never reaches a handler: LOST.
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
        // The loop sleeps until the recheck; that turn's drain finds nothing.
        let pass = watch.pass(recheck, [(3, &latch)]);
        assert_eq!(pass.healed, 1, "proven lost: healed");
        assert_eq!(latch.load(Ordering::Relaxed), 0);
        // The next burst posts a fresh wake.
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

    /// An unsent arm (between arm and post) is never presumed lost or stranded,
    /// however old: its post is still coming. It rechecks after one expiry while
    /// young and after its own age once older, so a stalled post never becomes a
    /// periodic wake-up of an idle loop.
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

    /// A late wake is booked once however many turns it waits; a new wake on
    /// the same session is a new book.
    #[test]
    fn a_late_wake_is_booked_once() {
        let latch = AtomicU64::new((10 * MS) | WAKE_LATCH_SENT);
        let mut watch = OutputWakeWatch::default();
        // Every pass stamps at the latch or before it, so it is never proven lost.
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

    /// Build the APC `G` sequence for an `a=T` transmit with `control` keys and a
    /// base64-encoded `payload` (here, a file path / shm name).
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

    /// The foreground handback restores 1007 to the host's on default, so it
    /// never turns wheel scrolling in `less`, `man` or `git log` off.
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
        // A program that switched it off and died gets it switched back ON.
        t.process(b"\x1b[?1007l\x1b[?1049h");
        t.foreground_handback().expect("the alt screen is evidence");
        assert!(t.modes().alternate_scroll);
    }

    /// Every live session's alt-screen archive carries this process's origin: set
    /// (a bare `Terminal::new` leaves it 0), the same for two sessions of one
    /// process (the id is the process, not the session), and stable across calls.
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

    /// SCROLL-1 — a fresh live session attaches a tiered scrollback STORE (not the bare
    /// 10k ring), and the resolved total `scrollback_limit` actually drives ring + store
    /// — the setting that was a silent no-op while `grid.scrollback` stayed `None`.
    /// Covers a finite cap and `0` ⇒ unlimited (line limit `None`).
    #[test]
    fn live_session_attaches_tiered_scrollback_driven_by_config() {
        use aterm_core::config::TerminalConfig;
        // No config: the store is present.
        let t = new_live_terminal(10, 40, None, None);
        assert!(
            t.scrollback().is_some(),
            "SCROLL-1: a live session must attach a tiered scrollback store"
        );
        // A finite config limit reaches the store (was a no-op with scrollback = None).
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
        // `scrollback_lines = 0` ⇒ the GUI resolver maps it to `None`, which the store
        // reads as unlimited (bounded only by the memory budget).
        let tc = TerminalConfig {
            scrollback_limit: None,
            ..TerminalConfig::default()
        };
        let t = new_live_terminal(10, 40, Some(&tc), None);
        // Store still attached (so the `None` below is "unlimited", not "no store" —
        // `scrollback_line_limit()` collapses both to `None`).
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

    /// BROKEN-2 — a session built from a config whose `color_scheme` is LIGHT (a
    /// light palette) reports the LIGHT scheme from the start (engine
    /// `color_scheme()` + the DSR `CSI ?996n` reply), instead of the engine's
    /// `Dark` default. This is the tab/split-spawned-after-attach case the
    /// finding reported: the pixels were light but the engine reported dark.
    #[test]
    fn live_session_adopts_the_os_color_scheme_at_construction() {
        use aterm_types::Appearance;
        // NEG: no config keeps the engine default, dark.
        let mut dark = new_live_terminal(10, 40, None, None);
        dark.process(b"\x1b[?996n");
        assert_eq!(dark.take_response().as_deref(), Some(&b"\x1b[?997;1n"[..]));
        // A light palette: the fresh engine must REPORT light, not the dark
        // default, from the config it is built with.
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
        // And the DSR `CSI ?996n` query answers light (Ps=2), agreeing with the pixels.
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

    /// A Kitty file medium is never read inside `process()`, which runs under
    /// the terminal lock (2026-10-14): a read that blocks (an evicted iCloud
    /// file downloading, a stalled network volume) held the lock every
    /// window's redraw takes. The reader reads before its hold and the image is
    /// placed from that read, where the command stood; fed with no read made
    /// first, as a hold alone feeds it, the file is not read and the image is
    /// refused.
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

    /// The session's real PTY reader (`attach_reader_inner`, on a real
    /// pseudo-terminal) reads a Kitty file medium before the hold that feeds
    /// its command, so the image is placed; and it reads what the session
    /// admits as its last hold left it, so a resolver installed after the
    /// reader started is honoured once a hold has seen it. The other tests
    /// here feed as the reader does; this one is the reader.
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
        // The program writes `bytes` and ends them with a title, which the
        // engine applies after them: once it shows, they have all been fed.
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

    /// A directory of a test's own, removed with everything in it when the
    /// test ends, however it ends.
    struct Scratch(std::path::PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A fresh [`Scratch`] under `parent`.
    fn scratch_in(parent: &Path, tag: &str) -> Scratch {
        let dir = parent.join(format!("aterm-kft-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    /// A fresh directory outside every temporary one [`kitty_temp_dirs`] names, and its
    /// resolved path: in the build's target directory, else (unix) `/var/tmp`.
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

    /// A one-cell (10x20) RGBA image file at `dir/name`.
    fn cell_file(dir: &Path, name: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, vec![0u8; 10 * 20 * 4]).unwrap();
        path
    }

    /// Transmit and display `path` over `t=t` through the real resolver, and
    /// report whether the image was read and placed.
    fn transmit_temp_file(path: &Path) -> bool {
        let term = term_with_resolver();
        let seq = apc("a=T,f=32,s=10,v=20,t=t", path.to_str().unwrap().as_bytes());
        crate::host_io_prefetch::feed_as_reader(&term, &seq);
        !term_lock(&term).cell_frame(24, 80).images[0].is_empty()
    }

    /// `t=t` reads any file `t=f` would, and deletes only what the protocol
    /// lets it delete: a file whose path carries `tty-graphics-protocol`, in a
    /// temporary directory. The path is what a program wrote to the terminal,
    /// so without the rule any file the user can write could be deleted.
    #[test]
    fn a_temp_file_is_deleted_only_where_the_protocol_allows() {
        const MARKER: &str = "tty-graphics-protocol";
        let temp_scratch = scratch_in(&std::env::temp_dir(), "t");
        let temp = &temp_scratch.0;
        let (_outside_scratch, canonical) = outside_temp_dirs("kept");
        let outside = &canonical;

        // No marker, in a temporary directory: read and placed, as kitty reads
        // it, but kept.
        let plain = cell_file(temp, "plain.rgba");
        let placed = transmit_temp_file(&plain);
        assert!(
            plain.exists(),
            "a t=t file without the marker is never deleted"
        );
        assert!(placed, "a t=t file without the marker is still read");

        // Marker, in a temporary directory: read, placed and consumed.
        let consumed = cell_file(temp, &format!("x-{MARKER}.rgba"));
        assert!(transmit_temp_file(&consumed), "a proper t=t file is placed");
        assert!(!consumed.exists(), "a proper t=t file is deleted");

        // Marker, outside every temporary directory: read, but kept.
        let kept = cell_file(outside, &format!("x-{MARKER}.rgba"));
        let placed = transmit_temp_file(&kept);
        assert!(
            kept.exists(),
            "a file outside the temporary directories is kept"
        );
        assert!(placed, "the file is still read");

        // A path that starts in a temporary directory and climbs out of it with
        // `..` is judged where it lands.
        let climb = temp.join(MARKER);
        std::fs::create_dir_all(&climb).unwrap();
        let victim = cell_file(outside, "victim.rgba");
        let mut escaped = climb;
        for _ in 0..64 {
            escaped.push("..");
        }
        // Down again from the root: the target's path without its root (and,
        // on Windows, without its drive prefix; the two share a drive).
        escaped.extend(
            canonical
                .components()
                .filter(|part| matches!(part, std::path::Component::Normal(_))),
        );
        escaped.push("victim.rgba");
        let placed = transmit_temp_file(&escaped);
        assert!(victim.exists(), "`..` cannot carry the delete out of /tmp");
        assert!(placed, "the file is still read");

        // The marker counts only in the path of the entry deleted: a marked
        // directory a path passes through on its way to an unmarked file in
        // the same temporary directory does not lend it the mark.
        let unmarked = cell_file(temp, "unmarked.rgba");
        let lent = temp.join(MARKER).join("..").join("unmarked.rgba");
        let placed = transmit_temp_file(&lent);
        assert!(
            unmarked.exists(),
            "a passing marked directory lends no mark"
        );
        assert!(placed, "the file is still read");
    }

    /// The directory a `t=t` entry is judged in is the one it is unlinked from:
    /// a program that swaps the marked directory for a symlink after the
    /// judgement and before the unlink, as one that can write the temporary
    /// directories can, does not carry the delete out of them.
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

    /// The reply to an `a=t` transmission of `path` over `medium` (`f`, `t`).
    fn transmit_reply(term: &Arc<Mutex<Terminal>>, medium: &str, path: &Path) -> String {
        let control = format!("a=t,f=32,s=10,v=20,i=9,t={medium}");
        crate::host_io_prefetch::feed_as_reader(
            term,
            &apc(&control, path.to_str().unwrap().as_bytes()),
        );
        let reply = term_lock(term).take_response().unwrap_or_default();
        String::from_utf8_lossy(&reply).into_owned()
    }

    /// A Containment tab with `allow_kitty_file_transfer` on gets no Kitty
    /// medium resolver ([`host_reach`]): aterm reads no file and no shared memory
    /// its program names, so a file the profile refuses the program is neither
    /// drawn nor told apart from a missing one, and a `t=t` file is not deleted
    /// as aterm. A tab of any other mode reads the file, tells it apart from a
    /// missing one and consumes the `t=t` file, which is what makes the refusal
    /// mean something.
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

    /// A `t=t` FILE OR `t=s` OBJECT THE ENGINE REFUSES IS LEFT WHERE THE PROGRAM
    /// PUT IT (ATERM-KI-1014-6): the reader reads a medium before its hold, and
    /// the engine refuses a raw transmission that declares no size before it
    /// asks for the bytes; before, the read had already deleted the file and
    /// unlinked the object. CONTROL: the same transmission with its size
    /// declared is placed, and its medium consumed.
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
            // One page, as a 16-row RGBA raster: Darwin reports a shared-memory
            // object's size in whole pages.
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
        // Over the cap → resolver returns None → fail closed (nothing placed).
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

/// R08: the render debt an adopted session brought across, on the restored
/// engine and in the resize ledger.
#[cfg(test)]
mod adoption_render_debt_tests {
    use std::sync::{Arc, Mutex};

    use aterm_core::terminal::{RenderDebt, Terminal};

    use super::seed_adopted_render_debt;
    use crate::resize_epoch::EpochVerdict;
    use crate::session_timeline::SessionTimeline;

    /// A restored alternate-screen engine, as `hydrate_adopted_engine` leaves it.
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

    /// The seeded debt opens an epoch at attach: the risk wait runs from it, and
    /// once it has passed a Claude Code tab reads `desync-risk` and is the
    /// nudge's to repair; before it, the tab is only `displaced`.
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
        // The presence classifier reads Claude Code (the sweep names it soon
        // after attach); the ledger reads the class at each look.
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

    /// A carried cut (S2R-F3: a column flap cut drawn cells in the parent) is
    /// judged from attach as rows are: the nudge is owed though no row moved.
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

    /// Inside a bundle, an uncontained session with no inherited values gets
    /// both: the bundle's `aterm-askpass` and an empty `DISPLAY`.
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

    /// A user's own `SUDO_ASKPASS` is kept (not emitted), and a real `DISPLAY`
    /// is kept — so neither overwrites the inherited value.
    #[test]
    fn keeps_the_users_own_values() {
        if !cfg!(target_os = "macos") {
            return;
        }
        let env = askpass_env(Some(BUNDLE), false, Some("/opt/my/askpass"), Some(":0"));
        assert!(env.iter().all(|(k, _)| k != "SUDO_ASKPASS"), "{env:?}");
        assert!(env.iter().all(|(k, _)| k != "DISPLAY"), "{env:?}");
    }

    /// An enclosing aterm's `SUDO_ASKPASS` IS replaced with this instance's.
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

    /// No bundle (a dev `targo run`) and a contained session each set nothing.
    #[test]
    fn nothing_without_a_bundle_or_when_contained() {
        assert!(askpass_env(None, false, None, None).is_empty());
        assert!(askpass_env(Some(BUNDLE), true, None, None).is_empty());
    }
}
