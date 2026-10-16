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
use winit::event_loop::EventLoopProxy;

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

// Auto-prime: every fresh session spawn runs the coding-agent primer
// (`aterm agents install`) on a detached thread, throttled and fail-soft, so an
// agent launched anywhere knows what aterm is. `agents_auto_prime = false`
// turns it off.

/// How often one process re-runs the installer at most. A spawn burst (a
/// restore of twenty tabs) must cost one pass, not twenty; and after the first
/// pass every later one is a no-op re-read, so a minute loses nothing.
const AGENT_PRIME_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// When the installer last started in this process (`None` before the first
/// fresh spawn). Stamped before the thread runs, so a burst of spawns inside
/// the window never launches a second thread.
static AGENT_PRIME_LAST: Mutex<Option<Instant>> = Mutex::new(None);

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

/// Whether a fresh spawn may run the primer at all, pure: never in a unit
/// test (none may write the developer's real `$HOME`), never in a headless
/// instance. The throttle and the `agents_auto_prime` knob come after this.
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

/// Prime detected coding agents if the throttle says a pass is due; called at
/// the top of every fresh [`spawn_session`]. The pass runs on a detached thread
/// because the installer does disk I/O the event loop must not wait on. The
/// handle is dropped: nothing joins it, and the pass writes whole files only.
pub(crate) fn prime_agents_if_due() {
    // Unit tests construct `App`s that reach spawn seams, and headless
    // instances are tests and agents' private instances: neither may write
    // the `$HOME` it happened to inherit. A windowed aterm is where this runs.
    if !agent_prime_permitted(cfg!(test), HEADLESS_INSTANCE.load(Ordering::Acquire)) {
        return;
    }
    let now = Instant::now();
    {
        let mut last = AGENT_PRIME_LAST.lock().expect("agent prime throttle lock");
        if !due(*last, now) {
            return;
        }
        *last = Some(now);
    }
    let spawned = std::thread::Builder::new()
        .name("aterm-agent-prime".to_string())
        .spawn(|| {
            let enabled = crate::app_config::agents_auto_prime_setting();
            let _ = run_agent_prime(
                enabled,
                atpkg::hooks::runs_from_non_release_bundle(),
                aterm_primer::home_dir(),
            );
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
/// Detected from the selected shell (config `shell` / `--shell`) so a
/// non-default shell gets its own integration script; `detect_current()` sees
/// only the platform default (PowerShell on Windows).
pub(crate) fn detect_spawn_shell(
    shell_hint: Option<&str>,
) -> aterm_core::shell_integration::ShellType {
    use aterm_core::shell_integration as si;
    match shell_hint.filter(|s| !s.is_empty()) {
        Some(h) => si::ShellType::detect(h),
        None => si::ShellType::detect_current(),
    }
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

/// Prepare OSC 133/633 shell integration for the spawned shell: env additions,
/// an optional argv override (bash's `--rcfile`), and the raw capability nonce
/// for `Terminal::authorize_shell_integration`, so only this shell's marks are
/// trusted. `None` for an unknown shell or on I/O error; the shell still spawns
/// without command-block tracking, and the outcome is recorded
/// ([`ShellIntegrationOutcome`]). Runs in the parent, before spawn.
fn prepare_shell_integration(shell_hint: Option<&str>) -> Option<ShellIntegrationSetup> {
    use aterm_core::shell_integration as si;
    let shell = detect_spawn_shell(shell_hint);
    let mut injection = match si::prepare(shell) {
        Ok(Some(injection)) => injection,
        Ok(None) => {
            // Name the shell the way the USER selected it, so the diagnosis
            // reads as their own configuration and not our detection guts.
            let name = shell_hint
                .filter(|s| !s.is_empty())
                .map(String::from)
                .or_else(|| std::env::var("SHELL").ok())
                .unwrap_or_else(|| "the default shell".to_string());
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

/// The name of the interactive shell a fresh spawn runs, resolved the way the
/// spawn resolves it: config `shell` / `--shell`, else `$SHELL`, else
/// `/bin/sh` (Windows: its own default chain, `aterm_pty::select_shell_with_origin`).
fn interactive_shell_name(shell_override: Option<&str>) -> String {
    #[cfg(windows)]
    let program = aterm_pty::select_shell_with_origin(shell_override.map(std::ffi::OsStr::new)).0;
    #[cfg(not(windows))]
    let program: std::ffi::OsString = match shell_override.filter(|s| !s.is_empty()) {
        Some(s) => s.into(),
        None => std::env::var_os("SHELL")
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "/bin/sh".into()),
    };
    std::path::Path::new(&program)
        .file_name()
        .map_or_else(|| "shell".to_string(), |n| n.to_string_lossy().into_owned())
}

#[cfg(test)]
mod records_shell_start_tests {
    use super::records_shell_start;

    /// Only a fresh interactive shell records a start: `-e false` and an
    /// adopted shell close on exit exactly as before.
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

#[cfg(all(test, not(windows)))]
mod interactive_shell_name_tests {
    use super::interactive_shell_name;

    /// The name in `[fish ended at start: …]` is the program's basename, as a
    /// person would say it — never a path, never the login `-fish` spelling.
    #[test]
    fn names_the_program_the_spawn_runs() {
        assert_eq!(
            interactive_shell_name(Some("/opt/homebrew/bin/fish")),
            "fish"
        );
        assert_eq!(interactive_shell_name(Some("zsh")), "zsh");
        let default = interactive_shell_name(None);
        assert!(!default.is_empty() && !default.contains('/'), "{default:?}");
        assert_eq!(
            interactive_shell_name(Some("")),
            default,
            "an empty override is no override, as at the spawn"
        );
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
    /// shell-integration loader vars (which embed the per-tab nonce) are appended
    /// per session inside `spawn_session`, never here, so each tab's nonce is its own.
    pub(crate) env_add: Vec<(String, String)>,
    /// `-e <cmd>`: run this instead of `$SHELL` (also disables shell integration),
    /// as the OS's argv gave it ([`crate::cli::Cli::exec_command`]).
    pub(crate) exec_command: Option<Vec<std::ffi::OsString>>,
    /// Config `shell` / `--shell`: the interactive shell every tab spawns
    /// (discovery-resolved — `"bash"` finds Git Bash off-PATH, absolute paths
    /// verbatim). `None` → the platform default. See `aterm_pty` shell selection.
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
    pub(crate) terminal_config: Option<aterm_core::config::TerminalConfig>,
    /// The appearance of the palette on screen (`ResolvedColors::reported`), set
    /// on every new session's engine at construction (`Terminal::set_color_scheme`)
    /// so a tab spawned later reports the scheme its palette reads as to apps
    /// (DEC 2031 / DSR `CSI ?996n`). Seeded from the startup resolution and kept
    /// current by `App::install_colors`.
    pub(crate) appearance: aterm_types::Appearance,
    /// Whether to inject OSC 133/633 shell integration. Each tab gets a fresh
    /// CSPRNG nonce, authorized and required on its own engine, so one tab's
    /// output cannot forge another's marks. False when `-e` runs a command or
    /// integration is opted out.
    pub(crate) integrate: bool,
    /// Latency epoch shared across tabs: each session's PTY reader stamps the
    /// leading edge of its output bursts (`Session::last_output_ns`) on this
    /// origin, so the present path's output-to-present subtraction is valid.
    pub(crate) lat_epoch: Instant,
    /// The event loop's turn state, stamped on `lat_epoch` by the main thread
    /// (`App::new_events` / `window_event` / `about_to_wait`) and read by every
    /// tab's PTY reader: the one thing a reader needs to tell a `Wake::Output`
    /// lost together with its loop wake-up from one queued behind a long frame
    /// (see [`LoopIdle`] and [`gated_output_wake`]).
    pub(crate) loop_idle: Arc<LoopIdle>,
    /// Desktop-notification channel shared by every tab. Each session's engine
    /// notification callbacks (OSC 9/99/777) clone this sender; the delivery
    /// thread (`notify::spawn_delivery`) owns the receiver. The channel is bounded
    /// (`notify::NOTIFY_QUEUE_CAP`) and callbacks `try_send`, dropping on `Full`.
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
    /// [`crate::foreground_handback::group_gone`]: the group that lost the
    /// terminal is gone when its leader answers `ESRCH` to `kill(pgid, 0)` and no
    /// surviving member is stopped; any other owner only when its whole group is
    /// (`kill(-pgid, 0)`). Called as `fg_gone(master, pgid, role)`, only outside
    /// the term lock ([`crate::foreground_handback::FgOwners::probe_edge`]).
    pub(crate) fg_gone: crate::foreground_handback::FgGone,
    /// Overlap handoff (incoming side), armed like
    /// [`Self::defer_adopted_readers`]: re-key files of adopted shells are held
    /// here until the update's Commit (`shell_rekey::Deferred`), released after
    /// the reader gate opens. `None` otherwise: the adoption is final and writes
    /// at once.
    pub(crate) rekey_deferred: Option<Arc<crate::shell_rekey::Deferred>>,
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

/// Whether `s` is a well-formed session id (`s-` + 20 hex chars / 80 bits), the
/// shape [`SessionId::generate`] produces. Validates an injected id before
/// adoption, so a malformed `ATERM_SESSION_ID` falls back to a fresh identity.
pub(crate) fn is_valid_session_id(s: &str) -> bool {
    s.len() == 22 && s.starts_with("s-") && s.as_bytes()[2..].iter().all(u8::is_ascii_hexdigit)
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
fn adopt_injected_identity_in(
    dir: Option<&std::path::Path>,
    sid: Option<&str>,
    nonce_hex: Option<&str>,
) -> Option<(SessionId, LaunchNonce)> {
    let (sid, nonce) = parse_injected_identity(sid, nonce_hex)?;
    let dir = dir?;
    crate::identity_claim::claim_for_adoption(dir, &sid).then_some((sid, nonce))
}

#[cfg(test)]
mod injected_identity_adoption_tests {
    use super::adopt_injected_identity_in;
    use aterm_session::{LaunchNonce, SessionId};

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

        let first = adopt_injected_identity_in(Some(dir.path()), Some(&sid), Some(&hex))
            .expect("the first launch adopts the premint");
        assert_eq!(first.0, premint);
        assert!(first.1.ct_eq(&nonce), "the nonce rides with the id");

        // The first launch is still live (its claim parked) when the second reads
        // the same export; no clock enters the decision.
        assert!(
            adopt_injected_identity_in(Some(dir.path()), Some(&sid), Some(&hex)).is_none(),
            "a second launch from the same shell must NOT answer to a live id"
        );

        // Negative control: once the first launch releases its claim, the same
        // arguments adopt again (a relaunch).
        crate::identity_claim::release_claims_for_test();
        let relaunch = adopt_injected_identity_in(Some(dir.path()), Some(&sid), Some(&hex))
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
        assert!(adopt_injected_identity_in(Some(dir.path()), Some(&sid), None).is_none());
        assert!(adopt_injected_identity_in(Some(dir.path()), None, Some(&hex)).is_none());
        assert!(adopt_injected_identity_in(Some(dir.path()), Some("bad"), Some(&hex)).is_none());
        assert!(
            adopt_injected_identity_in(None, Some(&sid), Some(&hex)).is_none(),
            "no control directory, no claim, no adoption"
        );
    }
}

/// The capability tokens a parent minted for ONE child, kept so the parent can
/// later present them on the cross-process dial (Item 5's `ProxyTable`).
#[derive(Clone)]
pub(crate) struct ChildProvision {
    pub(crate) child_sid: SessionId,
    pub(crate) child_nonce: LaunchNonce,
    pub(crate) read: EdgeToken,
    pub(crate) write: EdgeToken,
    pub(crate) signal: EdgeToken,
}

/// The parent-side capability ([`crate::proxy::ProxyEntry`]) is exactly the
/// child's nonce + the three op tokens — derive it directly (both are `Copy`).
impl From<&ChildProvision> for crate::proxy::ProxyEntry {
    fn from(p: &ChildProvision) -> Self {
        crate::proxy::ProxyEntry {
            nonce: p.child_nonce,
            read: p.read,
            write: p.write,
            signal: p.signal,
        }
    }
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
        // The multiplexer baseline, stamped here rather than by the shell
        // integration: `aterm ctl` detects a multiplexer pane by comparing this
        // session's birth signature with the live one. A session whose shell
        // never sources the integration would otherwise inherit a pane's stamp
        // and be refused as a pane. Deny-listed with the other provisioning vars.
        (
            aterm_types::domain::ENV_MUX_BASE.to_string(),
            mux_env_signature_now(),
        ),
    ]
}

/// This process's multiplexer signature at the moment a session is born:
/// `"<$TMUX>|<$STY>"`, empty halves and all. Deliberately the raw pair rather
/// than a boolean — entering a DIFFERENT multiplexer later must read as a
/// crossing too, not merely "still inside one".
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
/// appended: the child installs no parent edges and the outer's `@child`
/// forward answers `ERR auth` (fail-closed). The tokens never enter the
/// inheritable environment (`aterm_spec::derive::capability_secrecy_model`). A
/// dir that refuses the file is logged.
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
             gets no parent edges, so `@{}` from this aterm answers ERR auth",
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

/// The whole recursion provisioning for one child: its adopted identity (env),
/// its edge secrets through the 0600 file ([`append_edge_token_channel`]), and
/// this aterm's capability over it registered for the cross-process proxy.
///
/// The returned [`proxy::ChildProxy`] owns that registration and the file and
/// retires both when dropped: at the session's close, or when a spawn fails
/// after provisioning.
pub(crate) fn provision_child_proxy(
    sock_dir: Option<&Path>,
) -> (Vec<(String, String)>, proxy::ChildProxy) {
    let (mut env, prov) = provision_child_recursion_env();
    let edge_file = append_edge_token_channel(&mut env, &prov, sock_dir);
    let child = proxy::ChildProxy::register(prov.child_sid.clone(), (&prov).into(), edge_file);
    (env, child)
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

/// Record the parent's preminted edges (from THIS process's injected env) into the
/// root session's edge table, so the outer aterm that spawned us holds the
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
    // The parent always mints all THREE ops (read/write/signal), so a child that
    // recorded fewer lost authority for some op — a malformed/duplicate/partial
    // injected token set. Surface ANY shortfall (n < 3), not only the all-missing
    // case, so a silent partial loss (e.g. two colliding hexes) is visible.
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
/// (`aterm_spec::derive::fd_handoff_no_leak_model`). An open un-adopted master
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
    /// The outgoing session's pool id, carried through so the incoming boot can place
    /// this adopted shell back into its ORIGINAL pane — the restore manifest's matching
    /// leaf records the same id (the layout↔live-fd bridge for a multi-session handoff).
    pub local_id: u64,
    /// The PTY master fd inherited across the exec (CLOEXEC was cleared before it),
    /// OWNED until [`spawn_session`] moves it into the session's sink: an `Adopted`
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
    /// The agent identity this shell was spawned under (`spawn identity=<name>`),
    /// from the handoff record. The shell keeps that identity's env; only the
    /// label rides here, onto `Session::identity` and the registry handle.
    pub identity: Option<String>,
    /// BROADCAST OPT-INS (round 23): the `topic add` set from the handoff
    /// record, `"<topic> <since>"` rows, already validated. Re-seeded onto the
    /// adopted session's `SessionFabric` so a seamless update does not silently
    /// unsubscribe an agent from the topic it is listening to.
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
    /// The build that handed this shell across (the manifest's
    /// `outgoing_build`; `None` from a producer that wrote none). Named by the
    /// handoff gap each of the session's recorders opens with
    /// ([`mark_handoff_gap`], round five, item 17).
    pub outgoing_build: Option<u64>,
    /// The PTY keeper's copy: `Some` for a master a crashed window left in the
    /// keeper's custody, with its release obligation
    /// ([`crate::keeper_link::Claim`]). [`spawn_session`] hands it to
    /// `keeper_link::register`; dropped unregistered, it releases the keeper's
    /// copy. `None` for an update's handoff.
    pub keeper: Option<crate::keeper_link::Claim>,
    /// THE RENDER DEBT (R08): `(shift, pushed)`, the rows the outgoing engine
    /// owed the alternate screen this session restores onto
    /// (`SessionRecord::render_debt`), only when non-zero and the screen was
    /// carried. [`spawn_session`] seeds the adopting engine with it
    /// (`Terminal::seed_render_debt`) and books the judgement of it from
    /// attach, so the epoch judge and the repaint nudge treat the tab exactly
    /// as they would have in the parent. `None`: owes nothing, or the producer
    /// predates the field, and nothing happens.
    pub render_debt: Option<(i32, i32)>,
    /// THE MAILBOX THE HANDOFF CARRIED (round seven of the update audit,
    /// findings 6, 8 and 28): the record's posts still waiting, receipts still
    /// owed and id counters, put back by [`seed_adopted_fabric`]. Empty for
    /// every fresh construction and from a producer that predates the carry.
    pub fabric: crate::fabric::FabricCarry,
    /// The record's newest timeline id (round seven, finding 50): the adopted
    /// timeline counts on above it ([`mark_handoff_gap`]). `0`: none carried.
    pub timeline_id: u64,
    /// The session log's epoch and node-id counters the record carried
    /// (`SessionRecord::log_ids`): the adopted log continues them
    /// ([`mark_handoff_gap`]). `None`: none carried (an older producer, a
    /// keeper's shell), and the log starts at its first epoch.
    pub log_carry: Option<crate::session_log::LogCarry>,
    /// The pair the crash journal image this window takes over at its Commit
    /// keys this shell by, when the outgoing process held an image that was not
    /// its own (`SessionRecord::journal_key`, round seven's leftover I2b).
    /// Named by this window's keeper registration and its own images
    /// ([`crate::keeper_link::Tag::keyed`]). `None` for everything else.
    pub journal_key: Option<(u32, u64)>,
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
/// session log's node ids continue the counters `log` carried.
pub(crate) fn mark_handoff_gap(
    timeline: &std::sync::Mutex<crate::session_timeline::SessionTimeline>,
    cast: &std::sync::Mutex<crate::cast::CastRecorder>,
    temporal: &std::sync::Mutex<crate::temporal::TemporalRecorder>,
    from_build: Option<u64>,
    timeline_id: u64,
    log: Option<crate::session_log::LogCarry>,
    keeper: bool,
) {
    let gap = crate::session_timeline::HandoffGap { from_build };
    {
        let mut tl = timeline.lock().unwrap_or_else(|p| p.into_inner());
        tl.continue_ids(timeline_id);
        tl.record("handoff", gap.payload());
        let at = tl.slog().now();
        tl.slog_mut().adopt(log, keeper, at);
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
    // Broadcast opt-ins the handoff carried: receiver-side consent for
    // `post to=say:<topic>` must survive a seamless update. Empty for fresh spawns.
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
/// 1. SCREEN CARRY: `checkpoint` restored (the exact pre-update screen, prompt
///    included), its carried nonce authorized
///    ([`authorize_adopted_shell_nonce`]), then the CONTROL CARRY's archive
///    installed into the engine that has just restored the screen it was
///    captured with. With no checkpoint the mark requirement is held standing
///    ([`hold_adopted_shell_nonce_requirement`]), and the archive is installed
///    all the same, without the differ's state (it describes a screen this
///    engine does not show): its rows are not about the screen, and a
///    session degraded for a meta this build could not read kept its ledger
///    but lost them, so `history`'s `arch=` marks read a fresh archive
///    (round eight, finding 73).
/// 2. THE RE-KEY CHANNEL (`shell_rekey`, 2026-09-24): `rekey` is the shell's
///    channel file when its handoff record said it has one. If the engine came
///    out of step 1 `Degraded` — no nonce crossed — a fresh nonce is written to
///    that file with NO engine lock held (file work), and only then authorized,
///    as a key the shell has not taken yet: marks signed with it verify from now
///    on, and `status integration=` reads `on` from the first one, not from the
///    write (a tab running a long program reaches its next prompt hours later).
///    No reader is attached yet, so no mark can land between the write and the
///    authorization. A shell without the channel stays `Degraded`, and says so.
///    UNDER AN OVERLAP HANDOFF (`deferred`, [`SessionFactory::rekey_deferred`])
///    the write waits for the update's Commit (review of 2026-09-25): the key is
///    authorized now and its file written only once the outgoing process has
///    committed, so an attempt abandoned after adoption — the old process
///    taking every session back — never moves a shell onto a key only the
///    abandoned candidate knew, nor overwrites a key the old process had
///    already handed its shell. Nobody holds a key before its file is written,
///    so authorizing it early lets through no mark.
///
/// 3. THE HISTORY CARRY's keys (`crate::handoff_history`, 2026-09-26):
///    `history`'s carried lines are coming after Commit, in front of the
///    history the checkpoint restored, so their absolute-row keys are reserved
///    now — in the same guard as the restore, before anything can read a key —
///    and the import later lands without moving a single live or retained
///    row's. The reserve's claim rides `history` to the import, which it fences:
///    a scrollback the adopted shell clears before the import stays cleared.
///    The bottom the restore left is noted beside it
///    ([`crate::handoff_history::AdoptedHistory::anchor_in`]), so the viewport
///    the import puts back is measured from the park's bottom, not from
///    wherever output has pushed it by then.
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

/// The temporal recorder's FIRST keyframe of session `id`'s engine, taken
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
/// ([`Terminal::set_console_host_posture`]), keyed on the same resize seam
/// policy the session resizes under ([`crate::app_render::pty_resize_policy`]),
/// so a `cast drift` replay folded under that policy rebuilds the same engine.
///
/// On a ConPTY session the kitty keyboard protocol is off: `CSI ? u` goes
/// unanswered and an application's push/set/pop is ignored, so keys stay
/// legacy VT (or the win32-input-mode records conhost asked for). And DEC 9001
/// is honored: conhost asks for those records at every ConPTY start. No other
/// session has a console host, so there the same request is a program's output
/// and changes nothing, whatever a hydrated checkpoint carried.
///
/// conhost forwards a program's kitty push but cannot turn our CSI-u reports
/// into the key records a console program reads, so keys such as Ctrl+L and
/// Alt+a would arrive as nothing. A program with ENABLE_VIRTUAL_TERMINAL_INPUT
/// loses only kitty's extras.
///
/// Gated on the backend (`aterm_pty::backend_is_conpty`), not `cfg(windows)`:
/// stub/replay sessions and Unix PTYs get the native posture. A conhost that
/// encodes key records as CSI-u would make this unnecessary for that backend.
pub(crate) fn apply_pty_keyboard_posture(term: &Mutex<Terminal>, master: i32) {
    let policy = crate::app_render::pty_resize_policy(master);
    term_lock(term).set_console_host_posture(policy);
}

/// The `rlimit` set a daily-driver (`user`/`master`) shell is spawned with:
/// the launcher's, unchanged — except the soft descriptor limit, when this
/// process raised its own to claim a launched handoff's grant (round six of
/// the update audit, item 39, review round two) or to park its sessions for an
/// update (round eight, #3). That raise is the update's need, not its shells':
/// a tab opened after an update through the launched lane, or after an
/// attempt from this process, gets back the soft limit the process was
/// launched with (launchd's 256), as a Dock-launched aterm's tab does. The
/// hard limit is never touched.
pub(crate) fn daily_driver_limits() -> aterm_sandbox::Limits {
    #[cfg(target_os = "macos")]
    let open_files = crate::handoff_rendezvous::launcher_soft_descriptor_limit();
    #[cfg(not(target_os = "macos"))]
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
    proxy: &EventLoopProxy<Wake>,
    // Per-spawn working directory: a new tab / split / window passes the focused
    // pane's OSC 7 cwd here without mutating the shared `factory`. `None` falls
    // back to `factory.cwd` (the `-d <dir>` flag, else the launch directory).
    cwd_override: Option<&str>,
    // Per-spawn CONTROLLER observation hint (session connections §2.3/§6):
    // `Some(origin)` injects `ATERM_OBSERVE_SESSION_ID=<origin sid>` into the
    // newborn's shell env (identity only, see [`provision_observe_env`]).
    // `None` for every non-connected spawn.
    observe: Option<&SessionId>,
    // `Some(name)` spawns the shell under that agent identity: each agent's home
    // variable points into `<state>/identities/<name>/<agent dir>` via `env_add`,
    // which the pty seam applies after the deny pass. The identity must already
    // exist (`agent_identity::ensure(name, false)`); a missing dir fails the
    // spawn. `None` is the user's own agent config. Ignored on an adopted shell
    // (its label comes from `Adopted::identity`).
    identity: Option<&str>,
    // `Some(_)` re-adopts a live shell handed across a seamless-update re-exec
    // (its PTY master fd, pid and identity) instead of forking a fresh one.
    mut adopt: Option<Adopted>,
) -> std::io::Result<Session> {
    // The coding-agent primer rides every FRESH spawn (an adopted shell is
    // already running; whatever agent it hosts was primed when it was first
    // spawned): throttled and detached, so it costs this spawn nothing.
    if adopt.is_none() {
        prime_agents_if_due();
    }
    // A recovered master's release obligation, held to the registration at
    // the end: any failure on the way drops it, which releases the keeper's
    // copy (`keeper_link::Claim`).
    let keeper_claim = adopt.as_mut().and_then(|adopted| adopted.keeper.take());
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
    // The carried broadcast opt-ins and halt, taken before the match below
    // consumes the handle; re-seeded onto `ctx.fabric` once it exists.
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
    // …and the session log's epoch and node-id counters.
    let adopt_log_carry: Option<crate::session_log::LogCarry> =
        adopt.as_ref().and_then(|adopted| adopted.log_carry);
    // …and the build that handed it across, for the handoff gap.
    let adopt_from_build: Option<u64> = adopt.as_ref().and_then(|adopted| adopted.outgoing_build);
    // …and the title the park captured (or the record names), for a
    // checkpoint without one.
    let adopt_title: String = adopt
        .as_ref()
        .map(|adopted| adopted.title.clone())
        .unwrap_or_default();
    // …and another supervisor's lease and the keyed escalations (round four,
    // item 9), re-seeded onto `ctx.meta` once it exists.
    let (adopt_supervisor, adopt_attention): (Option<String>, Vec<String>) = adopt
        .as_ref()
        .map(|adopted| (adopted.supervisor.clone(), adopted.attention_owners.clone()))
        .unwrap_or_default();
    // The identity LABEL this session wears: the name it is spawned under, or —
    // for an adopted shell, whose env is what it was — the one its record
    // carried across the handoff.
    let identity_label: Option<String> = match &adopt {
        Some(adopted) => adopted.identity.clone(),
        None => identity.map(str::to_owned),
    };
    // Per-tab shell integration: a FRESH nonce per session. Reusing a nonce
    // across tabs would let tab A's (untrusted) output emit tab B's authorized
    // OSC 133/633 marks; a distinct nonce per engine prevents that cross-tab
    // forgery. Computed only when integration is enabled (never under `-e`).
    let mut integrated_shell = None;
    let (mut env_add, argv_override, shell_nonce) = if adopt.is_some() {
        // ADOPTED session: the shell is ALREADY running, so nothing is injected into it
        // here — it keeps its ORIGINAL env (incl. the shell-integration nonce minted at
        // first spawn). No fresh nonce, no env, no argv override on this path.
        (Vec::new(), None, None)
    } else if factory.integrate {
        match prepare_shell_integration(factory.shell_override.as_deref()) {
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

    // Recursion provisioning: the root session adopts its fabric identity from
    // the injected env (so an outer aterm's preminted edges name us); other tabs
    // get a fresh one. Then mint a child identity and read/write/signal edges
    // for whatever this session spawns. This env is appended after the
    // shell-integration vars so it wins, and the deny-list strips inherited
    // copies so provisioning never replays past one hop.
    let (self_id, self_nonce) = match &adopt {
        // Adopted: restore the fabric identity from the handoff manifest so edges
        // and discovery stay valid. The id is kept unconditionally; the claim is
        // taken only once the predecessor has let go (`identity_claim::hold_transferred`).
        Some(a) => {
            crate::identity_claim::hold_transferred(&a.sid);
            (a.sid.clone(), a.nonce)
        }
        None if id == 0 => adopt_injected_identity()
            .unwrap_or_else(|| (SessionId::generate(), LaunchNonce::generate())),
        None => (SessionId::generate(), LaunchNonce::generate()),
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
        // CONTROLLER spawn: hand the supervisor the sid it observes (identity
        // only; adoption aside — a re-adopted shell keeps its original env).
        if let Some(origin) = observe {
            env_add.extend(provision_observe_env(origin));
        }
        // Identity env, applied by `build_child_env` after its deny pass, so the
        // parent's `CLAUDE_CONFIG_DIR`/`CODEX_HOME` never reach the child. Not
        // gated on `exec_command`. The dir must exist; a missing one fails the spawn.
        if let Some(name) = identity {
            let dir = crate::agent_identity::ensure(name, false)
                .map_err(|e| std::io::Error::new(e.kind(), format!("identity {name}: {e}")))?;
            env_add.extend(crate::agent_identity::env(&dir));
        }
        #[cfg(windows)]
        env_add.extend(tab_shell_env(
            factory.exec_command.is_some(),
            factory.shell_override.as_deref(),
        ));
    }
    // A one-shot `-e <cmd>` session never hosts an inner aterm, so skip child
    // recursion provisioning entirely — the injected tokens + the retained
    // `ProxyEntry` would be permanently unused. The session keeps the returned
    // `ChildProxy`, which retires the registration and the edge file when it drops.
    let child_proxy = if adopt.is_some() {
        // Adopted: the child keeps its first-spawn recursion tokens; env cannot be
        // re-injected, so nothing is re-provisioned.
        None
    } else if factory.exec_command.is_none() {
        // Edge-token secrets go to a 0600 file (path-only in env), and the
        // capability over the child is registered for the cross-process proxy.
        let (recursion_env, child) = provision_child_proxy(control_auth::socket_dir().as_deref());
        env_add.extend(recursion_env);
        Some(child)
    } else {
        None
    };

    // Seamless update: re-adopt the handed-off master fd and pid (the same shell
    // keeps running); otherwise fork a fresh shell.
    let adopted = adopt.is_some();
    // A fresh interactive shell records its start, so an exit moments later can
    // be told apart from one after it ran. Timed just before the fork: the
    // window starts no later than the shell does.
    let shell_started_at =
        records_shell_start(adopted, factory.exec_command.as_deref()).then(Instant::now);
    // `sink` is the master's one owner from here on ([`master_sink`]); `master`
    // stays only as the number the session resizes and records.
    // The foreground holder the outgoing reader last saw, from the handoff
    // record; `0` (fresh shell or nothing carried) means probe afresh.
    let carried_fg_holder = adopt.as_ref().map_or(0, |a| a.fg_holder);
    // THE RENDER DEBT the outgoing engine owed the screen (R08), seeded below.
    let carried_render_debt = adopt.as_ref().and_then(|a| a.render_debt);
    // THE HISTORY CARRY (`crate::handoff_history`): the sidecar opened before
    // the proof rides the session to Commit, when it is imported; its keys are
    // reserved as the engine is hydrated below. `None` for every fresh shell.
    let (master, sink, pid, adopt_checkpoint, mut adopt_control, adopt_repaint, mut adopt_history) =
        match adopt {
            Some(a) => {
                // FD HYGIENE: `into_sink` re-arms CLOEXEC (mirroring what forkpty
                // does for fresh masters) as it adopts the master.
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
                // Daily-driver modes (User, the default, and Master) inherit the
                // launching shell's limits so programs reserving large address
                // spaces are not constrained (`aterm_sandbox::Limits::inherit`);
                // Safety gets the soft caps, a guardrail its rc can raise, and
                // Containment the same caps as hard limits, which its program
                // cannot (`aterm_sandbox::Limits::contained`, ruling 2026-10-10).
                let limits = {
                    use aterm_containment::ContainmentMode as Cm;
                    match aterm_containment::mode_or_containment() {
                        Cm::Master | Cm::User => daily_driver_limits(),
                        Cm::Safety => aterm_sandbox::Limits::shell_default(),
                        // Fail-safe to contained for an unrecognized mode.
                        _ => aterm_sandbox::Limits::contained(),
                    }
                };
                // A folder the shell cannot start in becomes the home folder,
                // reported once the shell is up.
                let start =
                    crate::spawn_folder::start_folder(cwd_override.or(factory.cwd.as_deref()));
                // Capture the child pid (`spawn_shell_with_pid`) so `Session::drop` can HANG
                // UP the session (SIGHUP) before closing the master — the non-blocking
                // teardown that keeps the UI thread off the tty lock (see `Session::drop`).
                let spawned = aterm_pty::spawn_shell_with_pid_cell_px(
                    rows,
                    cols,
                    &factory.spawn_cap,
                    &factory.sandbox_cap,
                    &env_add,
                    factory.shell_override.as_deref(),
                    factory.shell_args.as_deref(),
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
                let aterm_pty::SpawnedShell { master, pid } = spawned?;
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
        name: interactive_shell_name(factory.shell_override.as_deref()),
        input: sink.driven_input(),
    });
    // Per-session asciicast v2 recorder, sized from this session's initial grid.
    // The header width/height are snapshotted here; resize events track changes.
    let cast = Arc::new(std::sync::Mutex::new(crate::cast::CastRecorder::new(
        cols, rows,
    )));
    // Per-session temporal recorder (B.9): the hydratable event-log spine.
    let temporal = Arc::new(std::sync::Mutex::new(
        crate::temporal::TemporalRecorder::new(),
    ));
    // Per-session live byte fan-out (Item 2): the reader thread tees every burst.
    let byte_fanout = Arc::new(crate::cast::ByteFanout::new());
    // Build the live engine (config applied, DEC 1007 alternate-scroll defaulted ON)
    // BEFORE the reader thread starts, byte-identical to the single-session startup.
    let term = Arc::new(Mutex::new(new_live_terminal(
        rows,
        cols,
        factory.terminal_config.as_ref(),
        factory.appearance,
        cell_px,
    )));
    // …and hand its lock-free input-mode mirror to the session context: the seam
    // reads it per key press instead of taking the terminal mutex.

    // Per-session fabric identity (day-one single local session: a fresh id+nonce).
    let ctx = Arc::new(SessionCtx {
        sink: sink.clone(),
        output_echo: Arc::new(crate::app_input::OutputEchoTracker::default()),
        modes: crate::mode_mirror_of(&term),
        ui_waiting: Arc::default(),
        edges: std::sync::Mutex::new(EdgeTable::new()),
        turn_lease: std::sync::Mutex::new(None),
        self_id,
        nonce: self_nonce,
        cast: cast.clone(),
        temporal: temporal.clone(),
        byte_fanout: byte_fanout.clone(),
        // An adopted session continues with the carried turn ledger (records
        // print `carried=1`), or an empty one naming the turn ids it no longer
        // vouches for. Other sessions start empty.
        turns: Arc::new(std::sync::Mutex::new(if adopted {
            crate::handoff_carry::adopted_ledger(adopt_control.as_mut())
        } else {
            crate::turn_ledger::TurnLedger::default()
        })),
        meta: std::sync::Mutex::new(crate::session_timeline::SessionMeta::default()),
        timeline: Arc::new(std::sync::Mutex::new(
            crate::session_timeline::SessionTimeline::default(),
        )),
        fabric: std::sync::Arc::default(),
        rewrap_gauge: std::sync::Arc::default(),
        human_input: Default::default(),
        generation_look: Default::default(),
        reset_lane: Default::default(),
        update_parked: Default::default(),
        fenced_writes: Default::default(),
    });
    // ROOT session only: record the edges the OUTER aterm preminted for us (from
    // our injected env), so it holds the read/write/signal authority it granted.
    if id == 0 {
        register_injected_parent_edges(&ctx);
    }
    // THE HANDOFF GAP (round five, item 17): an adopted session's recorders
    // start here, empty, and say so — first, before the fabric and claim
    // seeds below write their rows.
    if adopted {
        mark_handoff_gap(
            &ctx.timeline,
            &ctx.cast,
            &ctx.temporal,
            adopt_from_build,
            adopt_timeline_id,
            adopt_log_carry,
            recovered,
        );
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

    // SEAMLESS SCREEN CARRY: hydrate the adopted engine with the outgoing
    // process's checkpoint BEFORE the reader starts (no engine race) and before
    // the temporal keyframe (replay must reconstruct the restored screen, not a
    // blank one). This is what makes the post-update window show the exact
    // pre-update screen — prompt included — instead of booting blank over a
    // live shell that then LOOKS dead. The engine may restore at the OLD grid
    // size; the window's first resize converges engine + PTY to the new frame.
    //
    // Then the CONTROL CARRY's archive, into the engine that has just restored
    // the screen it was captured with: the archive goes on under its old origin
    // and indices, diffing the app's next frame exactly as the old process
    // would have. It never panics on what it is given, and without it the
    // restored screen is the new baseline, as before. A session adopted with
    // no screen still gets the archive's rows, without the differ's state.
    //
    // One lock acquisition covers both arms (`hydrate_adopted_engine`) so the
    // lock-order census, which reads lock lifetimes by brace depth, sees no
    // nested `term_lock`. `rekey_path` is the shell's re-key channel when its
    // record says it has one.
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
    }
    // THE RENDER DEBT (R08): the rows the outgoing engine owed this session's
    // alternate screen, on the restored engine, and the ledger's first look at
    // it - so the SAME epoch judge and repaint nudge that would have judged the
    // tab in the parent judge it here, the risk wait counted from attach. A
    // program that repaints on the pulse below (vim) clears it with the first
    // full clear as usual; a record that carried none seeds nothing.
    if let Some(debt) = carried_render_debt {
        seed_adopted_render_debt(&term, &ctx.timeline, debt);
    }
    // THE TITLE THE PARK CAPTURED (or the record names), for a checkpoint that
    // carried none (an older producer): without it the tab reads as its
    // directory until the program titles it again. Nothing for a fresh spawn
    // (its title is empty).
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

    // Temporal seed: record the initial keyframe before any PTY output, so
    // replay hydrates from it and folds `RawIn` events forward. Only when
    // temporal recording is enabled. The spine's resize watermark is seeded
    // from the same hold as the checkpoint, so the reader records every resize
    // after the keyframe and none it already holds (`crate::temporal::SpineWatermark`).
    if factory.temporal_recording {
        seed_temporal_keyframe(&term, &temporal, id);
    }

    // Trust ONLY this tab's command marks: install its FRESH nonce and require it.
    if let Some(nonce) = shell_nonce {
        let mut t = term_lock(&term);
        if !lost_nonce {
            t.authorize_shell_integration(nonce);
        }
        t.set_require_shell_integration_nonce(true);
    }

    // What aterm, unsandboxed, does at this tab's program's request: decided
    // once, from the mode its shell runs under ([`host_reach`]).
    let reach = host_reach(aterm_containment::mode_or_containment());
    configure_clipboard(&term, reach);
    configure_notifications(&term, reach, &factory.notify_tx, id);
    // Kitty file/shm transfer mediums — only when the user opted in (default off).
    if factory.allow_kitty_file_transfer {
        configure_kitty_file_transfer(&term, reach);
    }

    // POL-1: this tab's OWN `standard`-profile policy engine, installed BEFORE its
    // reader thread produces any bytes (same fail-closed posture as session 0).
    term_lock(&term).apply_policy_engine(aterm_policy::engine::PolicyEngine::new(
        aterm_policy::profiles::standard(),
    ));

    configure_bell(&term, proxy, id, window);
    configure_palette_refusal(&term);
    // XTWINOPS host transport (Linux): the engine gates on `allow_window_ops`
    // in-core; without this callback an authorized `CSI 9;1t` was a no-op.
    #[cfg(target_os = "linux")]
    configure_window_ops(&term, proxy, id, window);

    // Wake coalescing: at most one `Wake::Output` in flight per session. The
    // reader arms it on a clear-to-armed edge; the handler clears it before the
    // wake's work, so a chunk landing mid-handler re-arms. The value is the arm
    // timestamp (ns on the latency epoch; 0 = clear), so an unconsumed wake
    // expires instead of silencing the session (`gated_output_wake`).
    let output_wake_pending = Arc::new(AtomicU64::new(0));

    // Per-session output-burst stamp (leading-edge ns on `lat_epoch`; 0 = clear),
    // so the present path books output-to-present only for sessions visible in
    // the presenting window (`App::present_latency_ns`).
    let last_output_ns = Arc::new(AtomicU64::new(0));

    // Per-SESSION latest-output activity clock. Unlike `last_output_ns`, this is
    // overwritten for every reader burst and is never presentation-acknowledged:
    // automatic update admission uses its age so an old hidden-tab sample cannot
    // remain an eternal blocker merely because that tab never presents.
    let latest_output_activity_ns = Arc::new(AtomicU64::new(0));

    // Build the Session FIRST, then attach its byte pipeline: `attach_reader`
    // needs the assembled session (it is also the overlap handoff's resume and
    // deferred-adoption primitive, which only ever see a built `Session`).
    let mut session = Session {
        child_reaped: std::sync::atomic::AtomicBool::new(false),
        child_exit: std::sync::OnceLock::new(),
        exit_watch: None,
        shell_start,
        pty_closed_at: Arc::default(),
        id,
        term,
        vi_active: Arc::new(AtomicBool::new(false)),
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
        identity: identity_label,
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
        // A master of its own, fresh or adopted: counted in the PTY census.
        _pty_census: crate::metrics::PtyCensus::counted(),
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
        attach_reader(&mut session, window, proxy, factory).map_err(std::io::Error::other)?;
    }

    // An adopted alt-screen TUI restores as stale pixels it does not know to
    // repaint, so pulse the PTY size (rows-1, then rows) and the kernel delivers
    // SIGWINCHes. Main-screen shells skip it: their restored screen is exact.
    // A repaint adoption (blank engine by design) pulses on either screen.
    //
    // The pulse uses the engine's grid (the checkpoint's pane), not this
    // spawn's `(rows, cols)` (see `pulse_adopted_pty`), and is held only for a
    // repaint adoption so a size-comparing program sees it; an exact alt-screen
    // restore pulses back to back.
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
    // A REPAINT adoption's program may not answer the pulse (Claude Code reads
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
    crate::keeper_link::register(&session, keeper_claim);

    Ok(session)
}

/// Whether an adoption pulse holds its transient size ([`pulse_adopted_pty`]).
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PulseHold {
    /// Hold `rows-1` for [`ADOPTION_PULSE_HOLD`]: a REPAINT adoption, whose
    /// blank pane only a repaint fills.
    Held,
    /// The two halves back to back, as before the hold existed: an EXACT
    /// alt-screen restore, whose screen is already right. A program that
    /// redraws on any SIGWINCH (ncurses, vim) still does; a size-comparing one
    /// (Node) does nothing, which on an exact screen is correct.
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
/// engine's own grid, delivered as SIGWINCHes the program can see; for a
/// [`PulseHold::Held`] pulse, [`ADOPTION_PULSE_HOLD`] apart.
///
/// The engine's grid, not the spawn's `(rows, cols)`: a restored split pane is
/// spawned with the window's grid while its engine restores at the pane's
/// size, and no later winsize writer would correct it. Ending on the engine's
/// grid also resumes a rolled-back Commit at the program's own size.
///
/// The hold: a size-comparing program (Node's `tty.WriteStream`, so Claude
/// Code) reads `TIOCGWINSZ` on a later loop turn and repaints only if the size
/// differs from its cache, so back-to-back halves never force a repaint. A
/// repaint adoption (blank pane by design) therefore holds `rows-1` on the PTY
/// only; the engine keeps its grid, so no row is demoted or journaled. The
/// hold runs on a short-lived thread, off the event loop. An exact alt-screen
/// restore is not held ([`PulseHold::BackToBack`]), since drawing at `rows-1`
/// would show its layout one row short.
///
/// `adopt_pulse_hold` stands for the hold, set and cleared under the engine
/// lock; `sync_cell_pixel_size` leaves this PTY alone while it stands.
/// `resize_panes` writes only on a grid change, and the control `resize` verb
/// writes what it is told.
///
/// The second half writes the engine's grid as it is then, under the engine
/// lock, so a real resize during the hold is not undone; it writes through a
/// close-on-exec duplicate of the master, so a session closed during the hold
/// cannot hand the write to a reused fd. That duplicate keeps the master open
/// up to the hold's length after such a close.
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
        let spawned = std::thread::Builder::new()
            .name("aterm-adopt-pulse".into())
            .spawn(move || {
                // It takes the engine lock the UI thread contends: the floor.
                crate::qos::set_self(crate::qos::Role::Responsive);
                std::thread::sleep(ADOPTION_PULSE_HOLD);
                restore.run();
            });
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
    (shift, pushed): (i32, i32),
) {
    let probe = {
        let mut engine = crate::term_lock(term);
        engine.seed_render_debt(shift, pushed);
        crate::resize_ledger::EngineProbe::sample(&engine)
    };
    timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .evaluate_resizes(probe, crate::turn_ledger::now_ms());
}

/// Whether an ADOPTED session's PTY gets the rows-1 → rows size pulse that
/// makes the program in it redraw: when it restored onto the alternate screen
/// (a TUI whose last full redraw died with the old window), or when its screen
/// is blank on purpose (`Adopted::repaint` — the producer could not carry it, or
/// this build refused it and degraded the one session instead of the update,
/// the 2026-09-22/23 update audit). A main-screen shell restored exactly is
/// left alone: its line editor redrawing over an exact screen is only churn.
/// Named so the rule is tested without a PTY.
#[cfg(any(unix, test))]
fn adoption_needs_size_pulse(
    checkpoint: Option<&aterm_core::terminal::TerminalCheckpoint>,
    repaint: bool,
) -> bool {
    repaint || checkpoint.is_some_and(|cp| cp.modes.alternate_screen)
}

/// `spawn_session`'s source text, for the tests that pin what its body wires.
#[cfg(test)]
fn spawn_session_body() -> &'static str {
    let src = include_str!("spawn.rs");
    let start = src
        .find("pub(crate) fn spawn_session(")
        .expect("spawn_session is defined here");
    let body = &src[start..];
    &body[..body.find("\n}\n").expect("the body ends")]
}

#[cfg(test)]
mod adoption_wiring_tests {
    /// AUDIT #7 FINDING 48 — a FRESH spawn starts where
    /// `spawn_folder::start_folder` decided: the folder asked for (the
    /// override, else `-d`) goes through it, its `cwd` is what the PTY is
    /// handed, and the fall-back is said only once the shell is up (a failed
    /// spawn notes nothing). Checked by reading the source, as the pins below
    /// are: `spawn_folder`'s tests drive the decision and the queue, and
    /// without this the PTY could be handed the folder asked for while every
    /// one of them stayed green.
    #[test]
    fn spawn_session_hands_the_pty_the_start_folder_and_says_it_after_the_spawn() {
        let body = crate::spawn::spawn_session_body();
        let decided = body
            .find("crate::spawn_folder::start_folder(cwd_override.or(factory.cwd.as_deref()))")
            .expect("the start folder is decided from the override, else -d");
        let pty = body
            .find("aterm_pty::spawn_shell_with_pid_cell_px(")
            .expect("the fresh spawn");
        let args = &body[pty..pty + body[pty..].find(");").expect("the call ends")];
        assert!(
            args.contains("start.cwd.as_deref(),"),
            "the PTY gets it: {args}"
        );
        assert!(
            !args.contains("cwd_override"),
            "not the folder asked for: {args}"
        );
        let spawned = body.find("= spawned?;").expect("the spawn's result");
        let said = body.find("start.say();").expect("the fall-back is said");
        assert!(
            decided < pty && pty < spawned && spawned < said,
            "in that order"
        );
        assert_eq!(body.matches("start.say()").count(), 1, "said once");
    }

    /// `spawn_session` puts back EVERY adopted engine through
    /// `hydrate_adopted_engine` — the seam the re-key tests drive
    /// (`shell_rekey::tests`) — handing it the session's re-key channel, and
    /// restores or authorizes nothing on the side. Checked by reading the
    /// source, because a unit test cannot build the event-loop proxy
    /// `spawn_session` needs (the same source-wiring-guard idiom as
    /// `every_non_boot_spawn_site_hands_over_a_real_cell_box`): without it,
    /// deleting the call, or dropping the channel from it, would leave every
    /// re-key test green while no adopted shell is re-keyed.
    #[test]
    fn spawn_session_hydrates_every_adopted_engine_through_the_one_seam() {
        let body = crate::spawn::spawn_session_body();
        let calls: Vec<&str> = body
            .match_indices("hydrate_adopted_engine(")
            .map(|(at, _)| &body[at..(at + 400).min(body.len())])
            .collect();
        assert_eq!(calls.len(), 1, "one call, for every adopted engine");
        let args = &calls[0][..calls[0].find(");").expect("the call ends")];
        for arg in [
            "adopt_checkpoint",
            "adopt_control",
            "rekey_path",
            "rekey_deferred",
            "id",
            "adopt_history",
        ] {
            assert!(args.contains(arg), "the call hands over `{arg}`: {args}");
        }
        for inline in [
            "restore_checkpoint(",
            "authorize_adopted_shell_nonce(",
            "hold_adopted_shell_nonce_requirement(",
            "shell_rekey::hand_over(",
        ] {
            assert!(
                !body.contains(inline),
                "`{inline}` belongs to hydrate_adopted_engine, not beside it"
            );
        }
    }

    /// THE COMMIT LATCH IS WIRED WHERE THE SESSION IS BUILT (round seven, item
    /// 20): `spawn_session` sets `Session::handoff_commit` from
    /// `adopted_handoff_commit` over the factory's `rekey_deferred`, with the
    /// session's own `adopted` and `recovered` — once, in the struct it
    /// returns. The helper's decision and the drop it protects are driven by
    /// `session_teardown_identity_tests::a_session_built_through_the_spawn_s_latch_keeps_only_an_uncommitted_adopted_shell`;
    /// this pins the one line that connects them, which a unit test cannot
    /// reach (`spawn_session` needs an event-loop proxy). Without it, setting
    /// the field to `None` reopened the finding with every test green.
    #[test]
    fn spawn_session_hands_an_adopted_session_the_handoff_s_commit_latch() {
        let body = crate::spawn::spawn_session_body();
        let wire = "handoff_commit: adopted_handoff_commit(adopted, recovered, factory.rekey_deferred.as_ref()),";
        assert_eq!(body.matches("handoff_commit:").count(), 1, "one field init");
        assert_eq!(
            body.matches(wire).count(),
            1,
            "wired through the helper: {wire}"
        );
        // The two conditions are the session's own facts, bound once, above.
        let recovered = body
            .find("let recovered = keeper_claim.is_some();")
            .expect("recovered: a shell the PTY keeper kept");
        let adopted = body
            .find("let adopted = adopt.is_some();")
            .expect("adopted: a shell handed over by the handoff");
        let wired = body.find(wire).expect("wired");
        assert!(recovered < wired && adopted < wired);
    }

    /// THE HANDOFF GAP (round five, item 17): the timeline, the cast and the
    /// temporal recorder of an adopted session start empty in this process —
    /// nothing of them crosses the update — and each SAYS so. The timeline's
    /// first row is `handoff carried=0 from_build=<n>`, the cast header
    /// discloses it beside `aterm_truncated`/`aterm_dropped`, and `temporal
    /// status` reports it beside `dropped_events=`. An older producer that
    /// wrote no `outgoing_build` reads `from_build=-`. NEGATIVE CONTROL: an
    /// unmarked (fresh) recorder keeps its plain header and status byte for
    /// byte. And `spawn_session` marks every adopted session, before any other
    /// row reaches its timeline.
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
            super::mark_handoff_gap(&timeline, &cast, &temporal, from_build, 0, None, false);
            // …and a row recorded after it (the fabric seed, the registration).
            timeline
                .lock()
                .unwrap()
                .record("spawned", "state=spawning".to_string());
            let rows: Vec<(u64, &str, String)> = timeline
                .lock()
                .unwrap()
                .since(None)
                .map(|e| (e.id, e.kind, e.payload.clone()))
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

        // The seam is called by `spawn_session`, for an adopted session only,
        // before the fabric seed writes the first other row.
        let body = crate::spawn::spawn_session_body();
        let calls: Vec<usize> = body
            .match_indices("mark_handoff_gap(")
            .map(|(at, _)| at)
            .collect();
        assert_eq!(calls.len(), 1, "one mark, for every adopted session");
        let call = &body[calls[0]..];
        let args = &call[..call.find(");").expect("the call ends")];
        for arg in [
            "ctx.timeline",
            "ctx.cast",
            "ctx.temporal",
            "adopt_from_build",
            "adopt_timeline_id",
        ] {
            assert!(args.contains(arg), "the call hands over `{arg}`: {args}");
        }
        let guard = &body[..calls[0]];
        assert!(
            guard.trim_end().ends_with("if adopted {"),
            "only an adopted session is marked"
        );
        let fabric_at = body.find("seed_adopted_fabric(").expect("the fabric seed");
        assert!(calls[0] < fabric_at, "the handoff row is the first row");
    }

    /// `spawn_session` puts back EVERY adopted session's fabric — its topic
    /// opt-ins and its standing HALT — through `seed_adopted_fabric`, the seam
    /// `session_store`'s and `seamless`'s hold tests drive, and
    /// seeds nothing on the side. Pinned by source like the hydrate seam
    /// above: without it, deleting the call would leave every hold test green
    /// while each seamless update lifted every halt again (the round-four
    /// plan, item 3). And it runs inside `spawn_session`, which returns before
    /// its caller registers the session — no control verb can reach an
    /// adopted session before its halt is back.
    #[test]
    fn spawn_session_seeds_every_adopted_fabric_through_the_one_seam() {
        let body = crate::spawn::spawn_session_body();
        let calls: Vec<&str> = body
            .match_indices("seed_adopted_fabric(")
            .map(|(at, _)| &body[at..(at + 200).min(body.len())])
            .collect();
        assert_eq!(calls.len(), 1, "one seed, for every adopted session");
        let args = &calls[0][..calls[0].find(");").expect("the call ends")];
        for arg in [
            "ctx.fabric",
            "ctx.timeline",
            "adopt_topics",
            "adopt_holds",
            "adopt_fabric",
        ] {
            assert!(args.contains(arg), "the call hands over `{arg}`: {args}");
        }
        for inline in [
            "set_topics(",
            "seed_holds(",
            "parse_holds(",
            "parse_hold(",
            "register_session(",
        ] {
            assert!(
                !body.contains(inline),
                "`{inline}` does not belong in spawn_session beside the seed"
            );
        }
        // …and the record's title (item 8a), through its one seam, AFTER the
        // hydrate that decides whether the checkpoint carried a title.
        let title_at = body
            .find("seed_adopted_title(&term, adopt_checkpoint.as_ref(), &adopt_title)")
            .expect("spawn_session seeds the record's title through its one seam");
        let hydrate_at = body
            .find("hydrate_adopted_engine(")
            .expect("the hydrate call");
        assert!(hydrate_at < title_at, "the title seed follows the hydrate");
        // …and another supervisor's lease and the keyed escalations (item 9),
        // through the seam `session_store`'s carry test and its Tier-1 bind
        // drive, onto this session's own meta — deleting it would leave both
        // green while every update dropped the claims again.
        let claims: Vec<&str> = body
            .match_indices("seed_carried_claims(")
            .map(|(at, _)| &body[at..(at + 200).min(body.len())])
            .collect();
        assert_eq!(claims.len(), 1, "one claim seed, for every adopted session");
        let args = &claims[0][..claims[0].find(");").expect("the call ends")];
        for arg in [
            "ctx.meta",
            "ctx.timeline",
            "adopt_supervisor",
            "adopt_attention",
        ] {
            assert!(args.contains(arg), "the call hands over `{arg}`: {args}");
        }
    }

    /// THE BODY POINTER OF AN ADOPTED SHELL (2026-09-26) is written by the one
    /// spawn seam, for an adopted shell with a loader, naming the revision the
    /// carry brought (so a shell already on this build's body is left alone) and
    /// through the overlap handoff's Commit queue; and a FRESH shell is offered
    /// the pointer only when it is a zsh/bash/fish under integration. Pinned by
    /// source, as the hydrate seam above is: every `shell_body` test would stay
    /// green with the call deleted, while no adopted shell ever moved.
    #[test]
    fn spawn_session_points_every_adopted_loader_shell_at_this_builds_body() {
        let body = crate::spawn::spawn_session_body();
        let calls: Vec<&str> = body
            .match_indices("crate::shell_body::hand_over(")
            .map(|(at, _)| &body[at..(at + 200).min(body.len())])
            .collect();
        assert_eq!(calls.len(), 1, "one pointer write, at adoption");
        for arg in ["path", "id", "carried_rev", "rekey_deferred"] {
            assert!(
                calls[0].contains(arg),
                "the call hands over `{arg}`: {}",
                calls[0]
            );
        }
        let guard = &body[..body.find("crate::shell_body::hand_over(").expect("called")];
        let guard = &guard[guard.rfind("if adopted").expect("guarded by the adoption")..];
        assert!(
            guard.contains("body_loader"),
            "only a shell with a loader: {guard}"
        );
        assert!(
            body.contains("has_body_loader") && body.contains("crate::shell_body::POINTER_VAR"),
            "a fresh zsh/bash/fish is offered the pointer"
        );
    }
}

#[cfg(test)]
mod temporal_seed_tests {
    use super::seed_temporal_keyframe;
    use crate::temporal::TemporalRecorder;
    use aterm_core::terminal::Terminal;
    use std::sync::Mutex;

    /// An adopted engine can start mid-sequence (a split CSI) and its first
    /// keyframe carries the partial state, so replay finishes the sequence as
    /// the live engine does. Negative control: a hooked DCS query, which no
    /// keyframe can carry (its reply lives in the handler; a hooked string the
    /// engine only consumes, or a sixel, rides the carry as one to ignore since
    /// round eight's finding 57), leaves the recorder unseeded.
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
        Mutex::new(new_live_terminal(
            24,
            80,
            None,
            aterm_types::Appearance::Dark,
            None,
        ))
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

    /// `spawn_session` applies the posture to EVERY session it builds, once,
    /// after the hydrate (a restored checkpoint replaces the engine's modes,
    /// capability bit included) and before the reader can parse a byte.
    /// Checked by reading the source — a unit test cannot build the event-loop
    /// proxy `spawn_session` needs — the idiom of
    /// `spawn_session_hydrates_every_adopted_engine_through_the_one_seam`:
    /// without it, deleting the call would leave both tests above green while
    /// no live session got the posture.
    #[test]
    fn spawn_session_applies_the_posture_between_hydrate_and_reader() {
        let body = crate::spawn::spawn_session_body();
        let call = "apply_pty_keyboard_posture(&term, master);";
        assert_eq!(body.matches(call).count(), 1, "one call, for every session");
        let at = body.find(call).expect("called");
        let hydrate = body.find("hydrate_adopted_engine(").expect("hydrates");
        let reader = body.find("attach_reader(").expect("attaches its reader");
        assert!(hydrate < at, "the posture follows the hydrate");
        assert!(at < reader, "the posture precedes the reader");
    }
}

#[cfg(test)]
mod adoption_pulse_tests {
    use super::adoption_needs_size_pulse;

    /// The pulse rule, all four ways. The two REPAINT rows are the new ones:
    /// before them a blank-on-purpose adoption on the main screen — a shell
    /// whose carried screen the successor refused — sat on an empty pane until
    /// the program next happened to print.
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

    /// ROUND SIX, FINDING 2: a restored SPLIT pane running a TUI is spawned
    /// with the WINDOW's grid (24x80) while its engine restores at the PANE's
    /// (24x39). The pulse must leave the PTY at the pane's size — the engine's
    /// — or the program redraws at the window's width into half of it, and
    /// nothing later writes the pane size back (`resize_panes` sees the engine
    /// already at the pane rect). Driven over a real pty pair, read back
    /// through `TIOCGWINSZ` on the slave, as the program would.
    #[cfg(unix)]
    #[test]
    fn the_adoption_pulse_ends_on_the_engines_grid_not_the_windows() {
        let (_master_fd, _slave_fd, master, slave) = open_pty_pair();
        let pane = {
            let mut terminal = aterm_core::terminal::Terminal::new(24, 39);
            terminal.process(b"\x1b[?1049hvim");
            terminal.checkpoint_carry(0).expect("Ground")
        };
        // The spawn's engine is built at the WINDOW's grid, then restored.
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
        // HELD (a repaint adoption).
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
        assert!(
            pulsed.elapsed() >= super::ADOPTION_PULSE_HOLD,
            "the transient size was held, not taken back at once"
        );
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

    /// And `spawn_session` pulses through it: no `(rows, cols)` of the spawn
    /// reaches the adopted master's winsize.
    #[test]
    fn spawn_session_pulses_the_adopted_pty_at_the_engines_grid() {
        let body = crate::spawn::spawn_session_body();
        // Compared with whitespace removed: the formatter decides the wrapping.
        let compact: String = body.split_whitespace().collect();
        assert_eq!(
            compact
                .matches(concat!(
                    "pulse_adopted_pty(master,&session.term,",
                    "&session.adopt_pulse_hold,cell_px,hold,);"
                ))
                .count(),
            1,
            "the pulse reads the engine's grid and raises the session's hold"
        );
        assert!(
            compact
                .contains("lethold=ifadopt_repaint{PulseHold::Held}else{PulseHold::BackToBack};"),
            "only a repaint adoption holds its pulse"
        );
        assert!(
            !body.contains("aterm_pty::resize_with_cell_px("),
            "no winsize write from the spawn's own (rows, cols)"
        );
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

    /// How many threads are parked on this gate right now: each found it
    /// closed and unstopped, and can leave only by a release or its stop. A
    /// test waits for this FACT before it judges what a waiter has not done
    /// yet — instead of a guessed window that a slow start passes vacuously.
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
        AGENT_PRIME_INTERVAL, AgentPrimeOutcome, agent_prime_permitted, due, run_agent_prime,
    };
    use std::time::{Duration, Instant};

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

    /// The throttle: the first spawn is due, a burst inside the window is not,
    /// the window's edge is inclusive, and a clock step backwards is harmless.
    #[test]
    fn due_when_never_ran_or_once_the_interval_has_passed() {
        let t0 = Instant::now();
        assert!(due(None, t0), "nothing ran yet");
        assert!(!due(Some(t0), t0), "same instant");
        assert!(!due(
            Some(t0),
            t0 + AGENT_PRIME_INTERVAL - Duration::from_millis(1)
        ));
        assert!(
            due(Some(t0), t0 + AGENT_PRIME_INTERVAL),
            "edge is inclusive"
        );
        assert!(due(Some(t0), t0 + AGENT_PRIME_INTERVAL * 5));
        assert!(
            !due(Some(t0 + Duration::from_secs(1)), t0),
            "a clock that went backwards is not due, never a panic"
        );
    }

    /// One pass against a SCRATCH home: the knob is honoured before any read,
    /// a missing home is named, and a detected agent gets primed.
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

#[cfg(test)]
mod shell_integration_guard_tests {
    use super::SHELL_INTEGRATION_LOADED_GUARD;

    /// The spawn env overrides the inherited [`SHELL_INTEGRATION_LOADED_GUARD`]
    /// with an empty value, which defuses the loader guard only if the shipped
    /// scripts use exactly this name and test it with a non-empty check
    /// (`[[ -n "${…:-}" ]]`, safe under `set -u`). Pins both.
    #[test]
    fn nested_launch_guard_name_matches_the_shipped_scripts() {
        use aterm_core::shell_integration::scripts;
        let guard_test = format!("[[ -n \"${{{SHELL_INTEGRATION_LOADED_GUARD}:-}}\" ]]");
        for (shell, script) in [("zsh", scripts::ZSH), ("bash", scripts::BASH)] {
            assert!(
                script.contains(SHELL_INTEGRATION_LOADED_GUARD),
                "{shell}: the loader guard variable was renamed — update \
                 SHELL_INTEGRATION_LOADED_GUARD and the lib.rs spawn scrub"
            );
            assert!(
                script.contains(&guard_test),
                "{shell}: the loader guard is no longer a `[[ -n … ]]` check, \
                 so an empty-string override would not defuse it — rework the \
                 nested-launch scrub"
            );
        }
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

#[cfg(test)]
mod child_identity_env_tests {
    use super::{provision_child_identity_env, provision_child_recursion_env};
    use aterm_session::SessionId;
    use aterm_types::domain::{ENV_LAUNCH_NONCE, ENV_PARENT_SESSION_ID, ENV_SESSION_ID};

    fn keys(env: &[(String, String)]) -> Vec<&str> {
        env.iter().map(|(k, _)| k.as_str()).collect()
    }

    /// IDENTITY carries the parent id and NOTHING else — it is emitted for every
    /// session, including one-shot `-e <cmd>`, so it must grant no authority.
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

    /// The two provisioning halves must not overlap: recursion carries adoption
    /// identity (child sid + nonce) and must not emit the parent id, or a shell
    /// session would receive a duplicate key in `env_add`.
    #[test]
    fn recursion_env_no_longer_emits_parent_id() {
        let (env, _prov) = provision_child_recursion_env();
        let k = keys(&env);
        assert!(k.contains(&ENV_SESSION_ID), "adoption sid must remain");
        assert!(k.contains(&ENV_LAUNCH_NONCE), "adoption nonce must remain");
        assert!(
            !k.contains(&ENV_PARENT_SESSION_ID),
            "parent id moved to provision_child_identity_env; emitting it here too \
             would duplicate the key for shell sessions"
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

    /// The union is what a NON-exec (shell) session injects; it must still cover
    /// every var the recursion contract promised before the split.
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
/// `aterm_spec::derive::capability_secrecy_model` (`CapabilitySecrecy`): a
/// child's edge-token secrets are published through a 0600 file, never the
/// inherited environment.
///
/// [`provision_child_proxy`] is driven with a private socket dir and with none.
/// `published` is read off what it produced: `2` (env channel) if any of the
/// child's three secrets appears in an env value, `1` (file channel) if the env
/// names a file holding exactly them, else `0`. The secrets are the ones the
/// proxy table registered. The observed `Provision` must be a committed-model
/// step, and the file channel must be used exactly when there is a private dir.
///
/// `SandboxedRead` is not validated: the model derives the peer's holding from
/// the channel, so that step would check nothing `Provision` does not.
///
/// Negative control: putting the hexes into the env lands `Provision` on
/// `published = 2`, which only `Buggy = 1` takes.
#[cfg(test)]
mod capability_secrecy_conformance {
    use super::provision_child_proxy;
    use aterm_spec::verify::validate_transition_tiered;
    use aterm_types::domain::ENV_EDGE_TOKENS;

    #[test]
    fn edge_secret_provisioning_conforms_to_capability_secrecy() {
        let model = aterm_spec::derive::capability_secrecy_model();
        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let label = "CapabilitySecrecy(provision_child_proxy)";
        let dir = std::env::temp_dir().join(format!("aterm-f1-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch socket dir");
        for (what, sock_dir) in [("a private dir", Some(dir.as_path())), ("no dir", None)] {
            let (env, child) = provision_child_proxy(sock_dir);
            let entry = crate::proxy::lookup_child(child.sid()).expect("the child is registered");
            let secrets = [
                entry.read.to_hex(),
                entry.write.to_hex(),
                entry.signal.to_hex(),
            ];
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
        // The negative below is meaningful only once the waiter is provably
        // parked; a short `recv_timeout` alone would pass vacuously on a loaded box.
        let parked = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let waiter_parked = parked.clone();
        let join = std::thread::spawn(move || {
            waiter_parked.store(true, std::sync::atomic::Ordering::SeqCst);
            let _ = tx.send(waiter.wait_until_released(&waiter_stop));
        });
        while !parked.load(std::sync::atomic::Ordering::SeqCst) {
            std::thread::yield_now();
        }
        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(20))
                .is_err(),
            "closed gate must prevent pre-Commit activation"
        );
        gate.release();
        gate.release();
        assert_eq!(
            // Failure bound: a gate that never releases never sends. 1s covered a
            // condvar wake plus a thread's first schedule on a 40-thread box.
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
fn reset_reader_latches(session: &Session) {
    session.reader_stop.store(false, Ordering::Release);
    // Output is consumed again from here: the control waits measure idle and
    // submit windows on their own clocks once more (`SessionCtx::update_parked`).
    session.ctx.update_parked.store(false, Ordering::Release);
    // Relaxed matches the latch protocol everywhere else: grid content is
    // synchronized by the term mutex, the latch only governs wake delivery.
    session.output_wake_pending.store(0, Ordering::Relaxed);
}

/// Where a PTY reader posts its `Wake`s; `false` when the event loop is gone.
/// Shipping attaches use the event-loop proxy ([`post_to`]); the Tier-1 reader
/// bind (`dsu_quiescence_conformance`) uses its own sink. An `Arc` because the
/// attach also hands a clone to the sink's input hook, which posts
/// `Wake::InputWatch` from whichever thread wrote (`input_stall::watch_input`).
type WakePost = Arc<dyn Fn(Wake) -> bool + Send + Sync>;

/// The shipping [`WakePost`]: the event loop's proxy.
fn post_to(proxy: &EventLoopProxy<Wake>) -> WakePost {
    let proxy = proxy.clone();
    Arc::new(move |wake| proxy.send_event(wake).is_ok())
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
    let cast_tx = spawn_cast_writer(session.ctx.cast.clone())?;
    // The reader's handle on the recorder's DROP count and its epoch: taken
    // once here, so a failed `try_send` is counted (and later disclosed), and a
    // burst's time put on the recorder's timeline, without the recorder lock the
    // writer thread contends.
    let (cast_drops, cast_epoch) = {
        let rec = session.ctx.cast.lock().unwrap_or_else(|p| p.into_inner());
        (rec.drop_counter(), rec.epoch())
    };
    // The temporal writer thread and reader taps exist only when recording is
    // enabled; otherwise `None` and no writer thread is spawned.
    // Join any straggler writer from a prior attach (its reader was joined above)
    // before spawning a fresh one into the same recorder, so appends stay ordered.
    if let Some(old_writer) = session.temporal_writer_join.take() {
        let _ = old_writer.join();
    }
    let (temporal_tx, temporal_writer_join, spine_mark) = if factory.temporal_recording {
        // The spine records each resize's geometry only; the seam policy the
        // engine resizes with is the backend's, so the replay takes it from
        // here. Asked BEFORE the recorder lock: the answer takes the PTY
        // registry's own lock, which must not nest under this one.
        let policy = crate::app_render::pty_resize_policy(session.master);
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
    let (reply_tx, _reply_writer_join) = spawn_reply_writer(session.ctx.sink.clone())?;
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

    // MEM-L2 wake resources are created only after every helper thread exists. A
    // missing pipe is still safe (bounded-poll stop fallback); a reader-thread spawn
    // failure closes both raw descriptors before returning PreparationFailed.
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
        cast_tx,
        cast_epoch,
        cast_drops,
        temporal_tx,
        spine_mark,
        byte_fanout: session.ctx.byte_fanout.clone(),
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
        },
        timeline: session.ctx.timeline.clone(),
        reset_lane: session.ctx.reset_lane.clone(),
    });
    // ConPTY has no foreground process group: the Windows reader takes no probe
    // and keeps no holder.
    #[cfg(windows)]
    let _ = (factory.fg_probe, factory.fg_gone, &session.fg_holder);
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
    // the control waits before the reader is told (`SessionCtx::update_parked`).
    session
        .ctx
        .update_parked
        .store(true, std::sync::atomic::Ordering::Release);
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
        // Do not make a 1 ms sleep part of the event-loop latency contract: the
        // scheduler may oversleep it well past `deadline`. Yield, then re-read the
        // monotonic clock on every iteration.
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
    // loop now ends after draining its FIFO backlog. Observe it through the SAME
    // deadline before dropping its finished handle, so a re-attach cannot spawn a
    // second writer into the same recorder while this one is still appending.
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

        // The deadline is a failure bound only: a timeout would also return false
        // without exercising the panic path, so it is generous. A genuine failure
        // returns true.
        assert!(!park_reader(
            &mut session,
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        ));
        assert!(session.reader_join.is_none());
        // Parked, whatever the verdict: the control waits hold their clocks
        // (round seven, findings 62 and 63).
        assert!(
            session
                .ctx
                .update_parked
                .load(std::sync::atomic::Ordering::Acquire)
        );
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
        session
            .ctx
            .update_parked
            .store(true, std::sync::atomic::Ordering::Release);

        super::reset_reader_latches(&session);

        assert!(
            !session
                .ctx
                .update_parked
                .load(std::sync::atomic::Ordering::Acquire),
            "a re-attached reader consumes output again: the waits run on their clocks"
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
            || tx.send(()).is_ok(),
            || 1_002,
        );
        assert_eq!(
            rx.try_iter().count(),
            1,
            "the first burst after a re-attach must post its wake"
        );
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
    appearance: aterm_types::Appearance,
    cell_px: Option<(u16, u16)>,
) -> Terminal {
    // Attach a tiered scrollback store (hot / warm-LZ4 / cold-LZ4, bounded by
    // the memory budget) so `scrollback_lines` and the memory budget take
    // effect: without a store `apply_config`'s limit/budget setters no-op and
    // history is capped at the grid ring. The GUI uses only the in-memory
    // backend. `with_defaults()` seeds a 100k-line / 100 MB store; the limit is
    // one total across store and ring, so the no-config path applies the 100k
    // total minus the ring's capacity, and `apply_config` does the same split
    // (`scrollback_lines = 0` means unlimited, bounded by the budget). The ring
    // capacity comes from the byte target below.
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
    // Set the reported scheme (the palette's appearance, `factory.appearance`)
    // at construction so a tab spawned later reports it (DEC 2031 / DSR
    // `CSI ?996n`). A new session has no DEC 2031 subscriber yet, so no report
    // is queued; the live push is owned by `App::install_colors`.
    t.set_color_scheme(appearance);
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

/// Estimated byte target for a new live session's in-memory grid ring.
///
/// The ring holds recent scrollback uncompressed; older lines tier into the
/// attached store, whose line limit governs history depth.
///
/// A ring row is allocated at full grid width and recycled in place, so each
/// line costs about `cols × size_of::<Cell>()` + 45 B regardless of content.
/// A byte target keeps wide windows from spending tens of MB per tab on
/// uncompressed history the store holds far more cheaply.
///
/// This is a startup estimate, not a hard limit: allocator slack is not
/// modelled, the 512-line floor can exceed it at very wide widths, and resizing
/// keeps the chosen capacity. Checkpoint adoption uses `restore_checkpoint`'s
/// own 1000-line main-grid ring. A smaller ring pushes more rows through
/// conversion and LZ4; with a live compression worker that work is offloaded,
/// otherwise the inline drain on the reader path does it.
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

/// One OSC 52 write, already ROUTED to its platform destinations by
/// [`route_osc52_write`]: the text bound for the system CLIPBOARD and/or the text
/// bound for the X11 PRIMARY selection. `None` = "this destination untouched", so
/// a `'p'`-only set can never clobber the CLIPBOARD and vice versa.
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
enum HostReach {
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
/// RULING 2026-10-10 (P3 of the r13 policy hunt, widened on its review), fail
/// closed: aterm, unsandboxed, brokers nothing to a session spawned under
/// `Containment`. The profile refuses the contained program the pasteboard
/// (`docs/AUDIT-boundaries-2026-10-09.md` L1: the pasteboard hands data to
/// every other app), every file under the credential and private-data stores,
/// and every mach service but the user and group queries. aterm is a daemon
/// outside the sandbox that works for the program, so each of the following
/// handed the program what its profile refuses:
///
/// - OSC 52. The program set the system pasteboard by printing it (a planted
///   command waiting for the person's next paste into an unsandboxed shell)
///   and, under `allow_osc52_query`, read it back.
/// - Kitty `t=f`/`t=t`/`t=s` under `allow_kitty_file_transfer`. aterm read any
///   path the program named, so an `OK` against `EINVAL` told the program
///   whether a refused file existed, a raw format told its exact size, and its
///   bytes were drawn into the tab. A `t=t` also deleted a temporary file as
///   aterm.
/// - OSC 9/99/777 under `allow_notifications`. aterm posted the program's
///   text as a desktop notification: the program speaking outside its tab,
///   with no sign of which tab wrote it, through a notifier the profile
///   refuses it.
///
/// Decided here, once per session, where each broker is installed, so every
/// path that reaches one through the engine inherits it; the handlers carry no
/// second check. What the person does is not brokered at the program's
/// request and keeps working: a link click, a paste, a drop. The possible
/// later refinement for the clipboard is a session-scoped scratch clipboard
/// (`ATERM_DESIGN` §10.1, "OSC 52 → scratch clipboard"): the contained
/// program's copy kept in its tab, pasted only by the person.
///
/// Safety keeps the platform. It has no OS sandbox, so its program can run
/// `pbcopy`, read the file or post the notification itself. A mode this build
/// does not know is refused.
fn host_reach(mode: aterm_containment::ContainmentMode) -> HostReach {
    use aterm_containment::ContainmentMode as Cm;
    match mode {
        Cm::Master | Cm::User | Cm::Safety => HostReach::Platform,
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
fn configure_clipboard(term: &Arc<Mutex<Terminal>>, reach: HostReach) {
    install_clipboard(term, reach, deliver_clip_write);
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
    mut deliver: impl FnMut(ClipWrite) + Send + 'static,
) {
    if reach == HostReach::Refused {
        // No worker, no callback: `deliver` drops here, unused.
        let mut t = term_lock(term);
        t.revoke_clipboard_access(ClipboardAccess::Write);
        t.revoke_clipboard_access(ClipboardAccess::Query);
        return;
    }
    let (clip_tx, clip_rx) = clip_mailbox();
    std::thread::spawn(move || {
        // The mailbox keeps at most one pending update while a platform call is
        // blocked. Each selection is last-writer-wins, so a flood collapses to
        // the latest value per destination without retaining every payload.
        while let Some(write) = clip_rx.recv() {
            deliver(write);
        }
    });
    let mut t = term_lock(term);
    t.authorize_clipboard_access(ClipboardAccess::Write);
    // Off X11, `route_osc52_write` folds 'p' into the clipboard instead — the
    // routed `primary` half is then always `None`, which is what lets the worker
    // above compile the PRIMARY write out entirely on those platforms.
    let primary_supported = cfg!(target_os = "linux");
    t.set_clipboard_callback(move |op| match op {
        ClipboardOperation::Set {
            selections,
            content,
        } => {
            clip_tx.publish(route_osc52_write(&selections, &content, primary_supported));
            None
        }
        ClipboardOperation::Clear { selections } => {
            clip_tx.publish(route_osc52_write(&selections, "", primary_supported));
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
) {
    if reach == HostReach::Refused {
        return;
    }
    let mut t = term_lock(term);
    // Do not authorize here: `apply_config` set `modes.allow_notifications` from
    // config (default off), and the engine's OSC 9/99/777 handlers check that bit
    // before invoking these callbacks, so installing them unconditionally keeps
    // the opt-out intact.
    // OSC 9 / 777: a bare body string, no title.
    let tx = notify_tx.clone();
    t.set_notification_callback(move |body| {
        // try_send (never send): on a full BOUNDED queue this DROPS the message
        // instead of blocking the reader thread — bounding queue memory under a flood.
        let _ = tx.try_send(notify::NotifyMsg::program(id, None, body.to_string()));
    });
    // OSC 99 (kitty): structured title + body. Drop empty notifications
    // (close/update control frames with no content) rather than popping a
    // blank toast.
    let tx = notify_tx.clone();
    t.set_advanced_notification_callback(move |n| {
        if !n.has_content() {
            return;
        }
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
    proxy: &EventLoopProxy<Wake>,
    id: u64,
    window: WindowId,
) {
    let proxy = proxy.clone();
    term_lock(term).set_bell_callback(move || {
        let _ = proxy.send_event(Wake::Bell {
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
    proxy: &EventLoopProxy<Wake>,
    id: u64,
    window: WindowId,
) {
    let proxy = proxy.clone();
    term_lock(term).set_window_callback(move |op| {
        if let Some(wake) = window_op_wake(op, id, window) {
            let _ = proxy.send_event(wake);
        }
        // Manipulations answer no bytes; reports keep the engine's fallbacks —
        // so the callback's reply is `None` either way.
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
        // Reports (and any future operation): unanswered here — the engine's
        // in-core fallbacks own what can be answered honestly. `MoveWindow` is
        // denied in-core and can never arrive; the fallthrough keeps the
        // transport inert even if it somehow did.
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
///   ([`read_kitty_temp_file`]).
/// - `t=s` (shared memory): `shm_open(O_RDONLY)` + `mmap`, copy out, then
///   `shm_unlink`.
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
/// policy: `t=f` a regular file, `t=t` a temporary file (deleted as the protocol
/// allows), `t=s` a shared-memory object. Direct is the engine's own; any future
/// medium fails closed.
pub(crate) fn read_kitty_medium(
    medium: aterm_core::terminal::kitty_graphics::KittyMedium,
    name: &str,
) -> Option<Vec<u8>> {
    use aterm_core::terminal::kitty_graphics::KittyMedium;
    match medium {
        KittyMedium::File => read_kitty_file(Path::new(name)),
        KittyMedium::TempFile => read_kitty_temp_file(Path::new(name)),
        KittyMedium::SharedMemory => read_posix_shm(name),
        _ => None,
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

/// Read a Kitty `t=t` temporary file as `t=f` reads any file, and delete it
/// only where the protocol's security rule allows: in a known temporary
/// directory ([`kitty_temp_dirs`]), with [`KITTY_TEMP_FILE_MARKER`] in its
/// path. Any other file is read and kept, as kitty keeps it.
///
/// The path comes from the program in the tab, which may be a remote host or
/// a file being `cat`, so without this rule a transmission naming any file
/// the user can write deletes it.
fn read_kitty_temp_file(path: &Path) -> Option<Vec<u8>> {
    read_kitty_temp_file_with(path, || {})
}

/// [`read_kitty_temp_file`], running `before_unlink` once the entry has been
/// judged deletable and just before it is unlinked: the window a test swaps a
/// directory in.
fn read_kitty_temp_file_with(path: &Path, before_unlink: impl FnOnce()) -> Option<Vec<u8>> {
    let bytes = read_kitty_file(path)?;
    if let (Some(dir), Some(name)) = (path.parent(), path.file_name()) {
        consume_kitty_temp_entry(dir, name, before_unlink); // the client's temp file
    }
    Some(bytes)
}

/// Whether the protocol lets a `t=t` entry at `entry` (its directory resolved)
/// be deleted: [`KITTY_TEMP_FILE_MARKER`] in its path, under a
/// [`kitty_temp_dirs`] directory.
fn kitty_temp_entry_deletable(entry: &Path) -> bool {
    entry.to_string_lossy().contains(KITTY_TEMP_FILE_MARKER)
        && kitty_temp_dirs().iter().any(|tmp| entry.starts_with(tmp))
}

/// Delete `name` in `dir` if [`kitty_temp_entry_deletable`] allows it,
/// judging and unlinking in one directory.
///
/// RULING 2026-10-10 (P4 of the r13 policy hunt): the directory is resolved
/// once and held. `dir` is canonicalized (its symlinks and `..` resolved), and
/// that path is opened component by component from `/`, each relative to the
/// one before it with `O_DIRECTORY | O_NOFOLLOW`, so a symlink anywhere in it
/// refuses the open. The rule is judged on the path that was opened, and the
/// entry is unlinked relative to the held descriptor, never by path; a leaf
/// that is not a regular file is kept. Until this ruling the entry was judged
/// on a canonical path string and removed by that string, so its directory
/// components were resolved again at the unlink: a program able to write the
/// temporary directories (all a Containment shell can write) could swap a
/// component for a symlink in between, and aterm, unsandboxed, removed a
/// same-named file outside them. A swap now either makes the walk refuse or
/// moves nothing the held descriptor names. Since [`host_reach`] (same date) a
/// Containment tab gets no Kitty medium at all, so no shipped mode's program
/// has fewer rights than aterm here; the held directory stays as the rule for
/// any path aterm deletes on a program's word.
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
/// then `shm_unlink` it (the Kitty client expects the terminal to consume + remove
/// it). Returns `None` on any error. `unix`-only; a no-op stub elsewhere.
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
    // Remove the object name regardless (the client handed ownership to us).
    // SAFETY: `cname` is a valid C string; `shm_unlink` tolerates an absent name.
    unsafe { libc::shm_unlink(cname.as_ptr()) };
    result
}

#[cfg(not(unix))]
fn read_posix_shm(_name: &str) -> Option<Vec<u8>> {
    None
}

/// Bound on the asciicast writer's queue: the reader `try_send`s and drops on a
/// full queue, so a stalled writer cannot accumulate bursts (each up to a 64 KiB
/// PTY read) under an output flood. Recording is best-effort.
const CAST_QUEUE_CAP: usize = 1024;

/// Bound for the temporal-recorder writer queue (B.9), matching [`CAST_QUEUE_CAP`]:
/// a full queue drops the burst (best-effort recording) instead of growing without
/// bound while the writer thread is behind under a flood. See [`spawn_temporal_writer`].
const TEMPORAL_QUEUE_CAP: usize = 1024;

/// asciicast v2 writer thread: the reader hands program-output bursts here
/// lock-free, so JSON escaping and recorder locking never run on the reader's
/// hot path or under `term_lock`. Each burst carries its place in the engine's
/// order ([`crate::cast::CastCuts`]) and its arrival time, and the recorder
/// inserts it there, so a resize lands on the side the engine applied it. An
/// idle terminal sends nothing, so the thread parks on `recv()`.
fn spawn_cast_writer(
    cast: Arc<std::sync::Mutex<crate::cast::CastRecorder>>,
) -> Result<std::sync::mpsc::SyncSender<crate::cast::CastBurst>, String> {
    let (cast_tx, cast_rx) =
        std::sync::mpsc::sync_channel::<crate::cast::CastBurst>(CAST_QUEUE_CAP);
    std::thread::Builder::new()
        .name("aterm-cast-writer".into())
        .spawn(move || {
            // Holds the `cast` recorder mutex per burst, the same mutex the main
            // thread takes on every resize pass (`record_resize`), so qos.rs's
            // floor rule requires `Responsive`.
            crate::qos::set_self(crate::qos::Role::Responsive);
            while let Ok(burst) = cast_rx.recv() {
                let mut rec = cast.lock().unwrap_or_else(|p| p.into_inner());
                // Hand the reader's SHARED burst straight through: the common
                // complete-burst case retains the Arc (refcount bump) instead of
                // re-copying every output byte into the event deque; only a
                // burst a resize cut copies its pieces.
                rec.record_burst(burst);
            }
        })
        .map_err(|error| format!("spawn cast writer: {error}"))?;
    Ok(cast_tx)
}

/// Temporal recorder writer thread: the reader hands `RawIn`/`Reply` bursts
/// here lock-free, the same bounded/drop shape as [`spawn_cast_writer`], so the
/// spine append never runs on the reader's hot path or under `term_lock`. FIFO
/// preserves order; an idle terminal parks on `recv()`. The queue is bounded
/// ([`TEMPORAL_QUEUE_CAP`]); a full queue drops the burst (a recording gap).
fn spawn_temporal_writer(
    temporal: Arc<std::sync::Mutex<crate::temporal::TemporalRecorder>>,
) -> Result<
    (
        std::sync::mpsc::SyncSender<crate::temporal::TemporalMsg>,
        std::thread::JoinHandle<()>,
    ),
    String,
> {
    let (temporal_tx, temporal_rx) =
        std::sync::mpsc::sync_channel::<crate::temporal::TemporalMsg>(TEMPORAL_QUEUE_CAP);
    let join = std::thread::Builder::new()
        .name("aterm-temporal-writer".into())
        .spawn(move || {
            // The main thread waits on this worker (`park_reader` spins on
            // `is_finished()` under the handoff deadline), and control lanes take
            // the `temporal` mutex it holds per burst, so it runs `Responsive`.
            crate::qos::set_self(crate::qos::Role::Responsive);
            while let Ok(msg) = temporal_rx.recv() {
                temporal
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .apply(msg);
            }
        })
        .map_err(|error| format!("spawn temporal writer: {error}"))?;
    // The join handle lets `park_reader` drain this writer before a re-attach
    // spawns a fresh one into the same recorder, so events never reorder across
    // attaches. The `recv` loop ends once every sender drops (the reader's, at
    // thread exit), so the join is bounded.
    Ok((temporal_tx, join))
}

/// PTY query-reply writer thread: the reader hands DA/DSR/CPR replies
/// (`take_response()`) here lock-free and never writes them inline. An inline
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
        let queued = self
            .budget
            .try_reserve(reply.len())
            .is_some_and(|permit| self.tx.try_send(ReplyJob::Bytes(reply, permit)).is_ok());
        if !queued {
            crate::metrics::note_reply_dropped();
        }
        queued
    }
}

fn spawn_reply_writer(
    sink: Arc<SinkWriter>,
) -> Result<(ReplyLane, std::thread::JoinHandle<()>), String> {
    let (reply_tx, reply_rx) = std::sync::mpsc::sync_channel::<ReplyJob>(REPLY_QUEUE_CAP);
    // The spill-arranger `poke` installed below is a sender of this thread's
    // channel living inside the sink's `Shared`, so the thread holds the sink
    // only weakly: a strong clone would form a cycle in which `recv()` never
    // disconnects and every closed session leaks its writer thread, spill state
    // and PTY master. Pinned by `reply_writer_lifetime_tests`.
    let weak = Arc::downgrade(&sink);
    let join = spawn_reply_writer_thread(move || {
        // Holds `Shared.lock` — the mutex the keystroke write spins on — for
        // every reply it writes, so it must not run below the UI thread (the
        // qos.rs floor rule); at the inherited DEFAULT class a descheduled
        // hold turned into a keystroke's lost spin and a spill concession.
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
                // has still not been used (`ShellStart::input`).
                ReplyJob::Bytes(resp, permit) => {
                    let _ = sink.write_reply(&resp);
                    // Written (or failed with a dying session): the reply
                    // budget has these bytes back.
                    drop(permit);
                }
                ReplyJob::ArrangeSpillDrainer => sink.arrange_pending_drainer(),
            }
        }
    })
    .map_err(|error| format!("spawn reply writer: {error}"))?;
    // The UI thread's keystroke write concedes the fd lock to whoever holds it;
    // this thread takes the drainer-arrangement duty so a concession is a
    // VecDeque push plus a `try_send`, never a thread creation on the event
    // loop. A full queue drops the poke and the next conceding write re-pokes.
    // The slot is first-install-wins, so the first attach's writer carries the
    // duty for the sink's life.
    // Installed only after the thread exists: a failed spawn must leave the
    // slot empty for the retried attach's writer, or a poke into a
    // receiver-less channel would hold it and keystrokes would never be
    // delivered (`a_failed_spawn_leaves_the_arranger_slot_to_the_retried_attach`).
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
#[cfg(test)]
pub(crate) fn spawn_reply_writer_for_test(
    sink: Arc<SinkWriter>,
) -> (ReplyLane, std::thread::JoinHandle<()>) {
    spawn_reply_writer(sink).expect("spawn the reply writer")
}

/// The reply writer's ONE thread creation, behind a seam
/// `reply_writer_lifetime_tests` can fail on demand: thread exhaustion
/// (EAGAIN) is the failure `spawn_reply_writer` must leave no trace of, and it
/// cannot be provoked reliably in a test.
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
    /// Armed by a test on ITS thread: the next `spawn_reply_writer_thread` on
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

#[cfg(all(test, unix))]
mod reply_writer_lifetime_tests {
    //! The reply writer's spill-arranger duty must not pin the session: the
    //! arranger's sender lives inside the sink, so a writer holding the sink
    //! strongly would keep its channel, spill state and PTY master alive.
    use super::spawn_reply_writer;
    use aterm_session::sink::SinkWriter;
    use std::io::Read as _;
    use std::os::unix::net::UnixStream;
    use std::sync::Arc;
    use std::time::Duration;

    // A hang detector for things that must happen (a reply, EOF, a writer
    // exiting), never a latency budget.
    use aterm_spec::wait::MUST_HAPPEN;

    /// A master-owning sink over one end of a socketpair; the peer sees what it
    /// writes and EOF when the sink's `OwnedFd` closes.
    fn sink_and_peer() -> (Arc<SinkWriter>, UnixStream) {
        let (peer, writer) = UnixStream::pair().expect("socketpair");
        peer.set_read_timeout(Some(MUST_HAPPEN))
            .expect("read timeout");
        let owned: std::os::fd::OwnedFd = writer.into();
        (Arc::new(SinkWriter::new_owned(owned)), peer)
    }

    fn wait_finished(join: &std::thread::JoinHandle<()>, what: &str) {
        let exited = aterm_spec::wait::within(MUST_HAPPEN, Duration::from_millis(1), || {
            join.is_finished().then_some(())
        });
        assert!(exited.is_some(), "{what} never exited");
    }

    /// The class the writer actually runs at, asked of the kernel. The
    /// `Role::Interactive` line in [`spawn_reply_writer`]'s closure is the only
    /// thing setting it, and dropping it would compile and pass other tests
    /// while the thread holding `Shared.lock` fell to the background band.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_reply_writer_runs_at_the_class_the_keystroke_path_needs() {
        use std::os::unix::thread::JoinHandleExt as _;
        let (sink, mut peer) = sink_and_peer();
        let (reply_tx, join) = spawn_reply_writer(sink.clone()).expect("spawn");
        let thread = join.as_pthread_t() as libc::pthread_t;
        // The class is the inherited default until the closure's first statement
        // runs, so read it only once that statement has run — BY ORDER, not by
        // clock: the declaration comes before the writer's first `recv()`, so a
        // reply it was handed and wrote to the peer proves it ran.
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
    fn the_last_external_sink_clone_closes_the_master_and_ends_the_writer() {
        let (sink, mut peer) = sink_and_peer();
        let weak = Arc::downgrade(&sink);
        let (reply_tx, join) = spawn_reply_writer(sink.clone()).expect("spawn");
        assert!(reply_tx.submit(Arc::from(&b"ok"[..])), "queued");
        let mut buf = [0u8; 2];
        peer.read_exact(&mut buf).expect("the reply is written");
        assert_eq!(&buf, b"ok");
        // The reader exiting at EOF (its sender) and the session tearing down
        // (the ctx / mirror clones) — the MEM-L2 close path.
        drop(reply_tx);
        drop(sink);
        wait_finished(&join, "the reply writer");
        assert_eq!(
            weak.strong_count(),
            0,
            "no strong clone may survive the session"
        );
        let n = peer
            .read(&mut buf)
            .expect("EOF, not a read timeout: the master fd must have closed");
        assert_eq!(n, 0, "EOF");
    }

    #[test]
    fn a_reattached_writer_ends_with_its_reader_and_the_first_with_the_sink() {
        let (sink, mut peer) = sink_and_peer();
        let (first_tx, first_join) = spawn_reply_writer(sink.clone()).expect("spawn");
        let (second_tx, second_join) = spawn_reply_writer(sink.clone()).expect("re-attach");
        drop(first_tx);
        drop(second_tx);
        // The re-attach's writer holds nothing but its reader's sender: gone now.
        wait_finished(&second_join, "the re-attached reply writer");
        // The first carries the arranger duty for as long as the sink lives…
        std::thread::sleep(Duration::from_millis(20));
        assert!(
            !first_join.is_finished(),
            "the arranger-bearing writer must serve the live sink"
        );
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
        let err =
            spawn_reply_writer(sink.clone()).expect_err("the injected spawn failure surfaces");
        assert!(err.contains("spawn reply writer"), "{err}");
        assert!(
            !super::take_injected_reply_writer_spawn_fault(),
            "the fault is one-shot: consumed by the failed attempt"
        );
        // The retry — a fresh writer into the same sink.
        let (retry_tx, retry_join) = spawn_reply_writer(sink.clone()).expect("the retry spawns");
        assert!(retry_tx.submit(Arc::from(&b"ok"[..])), "queued");
        let mut buf = [0u8; 2];
        peer.read_exact(&mut buf)
            .expect("the retry's writer writes");
        assert_eq!(&buf, b"ok");
        drop(retry_tx);
        std::thread::sleep(Duration::from_millis(50));
        assert!(
            !retry_join.is_finished(),
            "the retried attach's writer must carry the arranger duty: a failed spawn \
             may not leave its dead sender in the slot"
        );
        drop(sink);
        wait_finished(&retry_join, "the retried reply writer");
        let n = peer
            .read(&mut buf)
            .expect("EOF, not a read timeout: the master fd must have closed");
        assert_eq!(n, 0, "EOF");
    }
}

// THRU-5: off-thread tier-compression worker tuning.
//
// A capacity-1 signal channel coalesces reader notifications: a token already
// queued means "a drain is pending", so a `try_send` drop is harmless.
const COMPRESS_SIGNAL_CAP: usize = 1;
/// The reader signals the worker once its deferred lazy backlog reaches this many
/// lines — below the 1000-line inline drain threshold, so the worker starts
/// promoting before the backlog grows large.
const COMPRESS_SIGNAL_AT: usize = 900;
/// Lines the worker promotes per term-lock hold. Small enough that the render
/// thread and PTY reader wait at most this batch's LZ4 cost (~a couple of block
/// compressions, sub-frame) per hold, so the former ~1000-line reader-thread
/// spike is smeared across many short worker-driven holds.
const COMPRESS_BUDGET: usize = 256;
/// The worker stops draining once the backlog falls to/below this small
/// amortization window, so it never thrashes the lock on the last few lines.
const COMPRESS_LOW_WATER: usize = 256;
/// Signals arriving within this window of each other mean the reader is
/// mid-flood; the worker defers promotion until the stream goes quiet.
const COMPRESS_QUIET_WINDOW: std::time::Duration = std::time::Duration::from_millis(50);
/// Mid-flood, promote at most ONE bounded batch this often — forward progress
/// on the backlog without measurably contending the flood's term-lock holds.
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
    std::thread::Builder::new()
        .name("aterm-scrollback-compress".into())
        .spawn(move || {
            // Background work that takes the `term` mutex the UI thread takes
            // on the keystroke path; being descheduled while holding it stalls
            // every waiter. `Responsive` (user-initiated) shortens that window
            // without outranking the UI thread.
            crate::qos::set_self(crate::qos::Role::Responsive);
            while rx.recv().is_ok() {
                // Flood gate: back-to-back signals mean the reader is mid-flood,
                // and promotion batches would contend the term lock with the PTY
                // drain. Wait for signals to go quiet, with one batch per
                // `COMPRESS_TRICKLE_INTERVAL` so a perpetual flood still progresses.
                // Memory stays bounded: past the staging cap the reader drops its
                // oldest staged lines (marked in history). The trickle is skipped
                // while the reader is cutting and a batch or more arrived since
                // the last one, so a flood leaves one marker, not one per batch
                // (`Grid::trickle_lazy_bounded`).
                let mut last_trickle = std::time::Instant::now();
                while let Ok(()) = rx.recv_timeout(COMPRESS_QUIET_WINDOW) {
                    if last_trickle.elapsed() >= COMPRESS_TRICKLE_INTERVAL {
                        term_lock(&term).trickle_lazy_bounded(COMPRESS_BUDGET);
                        last_trickle = std::time::Instant::now();
                        // Same slice-boundary handoff as the reader (P63).
                        crate::yield_to_ui_waiter(&ui_waiting);
                    }
                }
                // Timeout means quiet; Disconnected means teardown. Either way
                // drain below (the term Arc is still alive here).
                // Drain to the low-water mark in bounded, lock-yielding batches,
                // and stop when a batch makes no progress: while the store is
                // detached for an off-thread reflow `drain_lazy_bounded` is a
                // no-op, and waiting only for low water would spin the lock.
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
                    // Hand the boundary to a parked UI acquisition first (P63),
                    // then the historical scheduler yield.
                    crate::yield_to_ui_waiter(&ui_waiting);
                    std::thread::yield_now();
                }
            }
        })
        .ok()
        .map(|_| tx)
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

/// Batch messages from the gather stage to the parse stage (unix pipeline).
/// `pub(crate)` for [`crate::manual_reset::ResetLane`], which holds a clone of
/// the sender so a manual reset rides the same channel as the batches.
#[cfg(unix)]
pub(crate) enum GatherMsg {
    /// The deferred start gate released — the parse stage may post `Ready`.
    Started,
    /// One gathered batch (buffer, filled length, the foreground change it
    /// carries, and the group whose bytes END it — `edge.to` with an edge). The
    /// parse stage returns the buffer on the free channel after processing, and
    /// runs the foreground handback at the edge's offset.
    Data(
        Vec<u8>,
        usize,
        Option<crate::foreground_handback::FgEdge>,
        i32,
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
    // The manual reset's lane generation (`crate::manual_reset`), held for the
    // thread's life: its drop removes the lane's clone of `filled_tx` on every
    // exit path, so the channel ends with this thread and the parse stage's
    // `recv()` returns.
    lane_guard: crate::manual_reset::LaneGuard,
) -> Result<(), String> {
    std::thread::Builder::new()
        .name(format!("aterm-pty-gather-{id}"))
        .spawn(move || {
            let _lane_guard = lane_guard;
            // macOS: default QoS parks this thread on E-cores, whose wakeup
            // latency dwarfs the PTY producer/consumer cadence.
            crate::qos::set_self(crate::qos::Role::Responsive);
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
            let _ = filled_tx.send(GatherMsg::Started);
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
                // STOP before the next read, never between a read and its hand-off:
                // bytes already drained from the kernel MUST reach the engine (the
                // checkpoint-tear contract) — the parse stage always processes every
                // batch it was sent.
                if stop.load(std::sync::atomic::Ordering::Acquire) {
                    let _ = filled_tx.send(GatherMsg::Wake);
                    break;
                }
                let outcome = match after_exit {
                    None => aterm_pty::read_or_wake(master, &mut buf, wake_rd, exit_rd),
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
                    // The session ends with its shell, not with the slave's last
                    // close (docs/AUDIT-pty-render-2026-10-09.md P3): a job the
                    // shell left behind can hold the slave for hours, and Linux
                    // does not hang a PTY up at its session leader's exit. What
                    // the master holds goes on, then `Exited` reports the exit:
                    // the tab's close drops the session, whose wake stops this
                    // reader and whose master's close hangs up whatever still
                    // holds the slave. A pane `--hold` keeps goes on showing
                    // that job's output, and the job never blocks on a full,
                    // unread terminal.
                    aterm_pty::ReadOutcome::Exited => {
                        after_exit = Some(EXIT_DRAIN_MAX);
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
                        // Top up past the ~1 KiB kernel chunk, hand the batch over,
                        // then take a recycled buffer — blocking on the free channel
                        // IS the backpressure.
                        cutter.begin();
                        let filled = if nonblock {
                            aterm_pty::drain_more_nonblocking(
                                master,
                                &mut buf,
                                n,
                                wake_rd,
                                Some(parse_in_flight.as_ref()),
                                // The same interactive-input-pending hint the
                                // parse stage slices its lock holds on (P05).
                                crate::metrics::input_pending,
                                // The pre-park foreground sample: a change seen
                                // here delivers the batch NOW, cut at the last
                                // sample that still showed the old group.
                                &mut |f| cutter.sample(fg_probe(master), f),
                            )
                        } else {
                            // The blocking fallback never parks: batch end only.
                            aterm_pty::drain_more(master, &mut buf, n)
                        };
                        let edge = cutter.finish(|| fg_probe(master), filled);
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
                        // Counted BEFORE the send: the bridge can never see
                        // "idle" while a batch is queued unparsed, AND the
                        // reader's fetch_sub can never precede this add
                        // (usize underflow would seal the cutoff open).
                        parse_in_flight.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if filled_tx
                            .send(GatherMsg::Data(buf, filled, edge, holder))
                            .is_err()
                        {
                            break; // parse stage gone (session torn down)
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
            // The gather owns the wake pipe's READ end on unix — close it on exit.
            aterm_pty::close_fd(wake_rd);
        })
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
}

/// The owned wiring [`spawn_pty_reader`] moves into THIS session's reader thread:
/// the engine + the channels/proxy it feeds, plus the latency-stamp epoch. All
/// `Arc`/`Sender` clones are made by the caller so they are kept alive for the
/// thread's whole life (the channels stay open while the reader runs).
struct PtyReaderWiring {
    master: i32,
    id: u64,
    window: WindowId,
    term: Arc<Mutex<Terminal>>,
    /// Where this reader's `Wake`s go (the event loop, in every shipping attach).
    post: WakePost,
    /// Query-reply sink: the reader hands DA/DSR/CPR replies to the dedicated
    /// reply-writer thread over this FIFO instead of writing them inline (the
    /// inline write could block on the input pipe and deadlock the session).
    reply_tx: ReplyLane,
    /// THRU-5 compression-worker signal (`None` when the worker could not spawn ⇒
    /// offload inactive, reader drains inline). The reader `try_send`s a token
    /// after a burst once its lazy backlog crosses `COMPRESS_SIGNAL_AT`; the
    /// worker then promotes it into the compressed tiers off this critical path.
    compress_tx: Option<std::sync::mpsc::SyncSender<()>>,
    cast_tx: std::sync::mpsc::SyncSender<crate::cast::CastBurst>,
    /// The cast recorder's epoch ([`crate::cast::CastRecorder::epoch`]), copied
    /// once per attach: the reader puts each burst's arrival reading on it
    /// ([`crate::cast::CastBurst::new`], read outside every `term_lock` hold)
    /// without the recorder's lock.
    cast_epoch: Instant,
    /// The cast recorder's drop count ([`crate::cast::CastDrops`]): a burst
    /// `cast_tx` refuses is COUNTED here, so the recording discloses the gap
    /// (`aterm_dropped`) instead of replaying as if it held every byte.
    cast_drops: Arc<crate::cast::CastDrops>,
    /// `None` when temporal recording is disabled (the default): the reader then
    /// skips the `RawIn`/`Reply` spine taps entirely (no writer thread exists).
    /// BOUNDED ([`TEMPORAL_QUEUE_CAP`]) like `cast_tx`/`reply_tx` — the reader
    /// `try_send`s and drops on a full queue rather than blocking or growing unbounded.
    temporal_tx: Option<std::sync::mpsc::SyncSender<crate::temporal::TemporalMsg>>,
    /// The spine's resize watermark for this reader, attached to the session's
    /// ([`crate::temporal::SpineMark::attach`]): it starts where the previous
    /// reader left the spine. Unused (and unshared) when `temporal_tx` is `None`.
    spine_mark: crate::temporal::SpineMark,
    byte_fanout: Arc<crate::cast::ByteFanout>,
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
    /// Read between slices so the boundary becomes a handoff (P63).
    ui_waiting: Arc<std::sync::atomic::AtomicU32>,
    /// Read end of this session's wake pipe (MEM-L2): `Session::drop` writes the paired
    /// write end to break this reader out of a `read` that an orphaned child keeps from
    /// EOF'ing. `-1` when no pipe could be made ⇒ the wake-pipe-less fallback poll. Owned
    /// by the thread that reads the master (the gather on unix), which closes it on exit;
    /// when [`spawn_pty_reader`] returns `Err` no thread holds it and the caller closes it.
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
    /// The session's manual-reset lane (`crate::manual_reset`): the unix
    /// reader opens it with a clone of its gather→parse sender and closes it
    /// when either stage ends. Windows has no parse channel and leaves it
    /// closed, so a reset there runs directly under the term lock.
    reset_lane: Arc<crate::manual_reset::ResetLane>,
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
        cast_tx,
        cast_epoch,
        cast_drops,
        temporal_tx,
        spine_mark,
        byte_fanout,
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
        reset_lane,
    } = w;
    #[cfg(not(unix))]
    let _ = &reset_lane;
    // The parse stage's liveness probe (the orphan half of the foreground
    // handback). ConPTY has no foreground group, so off unix nothing is gone.
    #[cfg(unix)]
    let fg_gone = fg.gone;
    #[cfg(not(unix))]
    let fg_gone: crate::foreground_handback::FgGone = |_, _, _| false;
    // The unix pipeline's two stages: this thread becomes the parse stage, fed
    // recycled 64 KiB batches over a bounded channel by the gather. The gather
    // is wired here and spawned LAST: it owns the wake pipe's read end and
    // closes it on every exit, so it may run only once nothing after it can
    // fail. A gather-spawn failure drops its unrun closure, and with it the
    // batch sender and the lane guard: the parse stage's `recv()` returns
    // `Err` and it exits quietly, posting nothing.
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
        let (activity, stop, in_flight) = (
            latest_output_activity_ns.clone(),
            stop.clone(),
            parse_in_flight.clone(),
        );
        let start_gather = move || {
            spawn_pty_gather(
                master, wake_rd, shell_exit, id, lat_epoch, activity, stop, start_gate, read_buf,
                free_rx, filled_tx, in_flight, sink, fg.probe, fg.holder, lane_guard,
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
    let parse = std::thread::Builder::new()
        .name(format!("aterm-pty-reader-{id}"))
        .spawn(move || {
            // macOS: default QoS parks this thread on E-cores, whose wakeup
            // latency dwarfs the PTY cadence and costs drain throughput.
            crate::qos::set_self(crate::qos::Role::Responsive);
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
            // Accessed under `term_lock` with `process`; only this reader writes it.
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
            let mut ingest = |buf: &[u8],
                              edge: Option<crate::foreground_handback::FgEdge>,
                              holder: i32| {
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
                // taken outside every term-lock hold.
                let arrived = (!buf.is_empty()).then(Instant::now);
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
                // THRU-5: deferred-compression backlog after this burst (set under the
                // process lock below), read afterward to decide whether to wake the worker.
                let mut backlog = 0usize;
                // The foreground handback this batch ran, if any (see the slice loop).
                let mut handback: Option<aterm_core::terminal::ForegroundHandback> = None;
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
                // cut. Cost inside the lock: two loads and a compare per hold, no
                // clock read. A buffer is allocated only for a burst a resize cut.
                let mut cuts = crate::cast::CastCuts::default();
                let mut emitted = 0usize;
                let response = {
                    let bytes = buf;
                    // The manual reset, before any byte of `buf` and under one
                    // term-lock hold: the engine resets, the synthesized bytes go on
                    // the spine (geometry first), and what the reset cleared is
                    // credited to no holder.
                    if let Some(req) = pending_reset.take() {
                        let mut t = term_lock(&term);
                        cuts.observe(emitted, &t);
                        let spine_pre = temporal_tx
                            .as_ref()
                            .map(|_| crate::temporal::SpinePre::take(&t));
                        let (report, reset_bytes) = crate::manual_reset::apply(&mut t);
                        emitted += reset_bytes.len();
                        // Its assertions are taken here, as the automatic
                        // handback's are, so none is credited to the next
                        // slice's holder (a reset only disarms: none).
                        fg_owners.observe(holder, t.program_evidence(), t.take_evidence_asserted());
                        if !reset_bytes.is_empty()
                            && let (Some(tx), Some(pre)) = (&temporal_tx, spine_pre)
                        {
                            spine_mark.record(tx, pre, &t, &reset_bytes);
                        }
                        manual_run = Some((req, report, reset_bytes));
                    }
                    // Slice the burst so the term lock is released between chunks,
                    // choosing the width per hold:
                    //   * a keystroke waiting to echo (`metrics::input_pending`): 8 KiB
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
                    let mut off = 0;
                    // The handback offset, clamped to the batch; consumed once.
                    let mut handback_at = edge.map(|e| e.at.min(bytes.len()));
                    // Whose bytes the next slice holds: the old holder's before
                    // the cut, the batch-end holder's from it on.
                    let mut slice_holder = edge.map_or(holder, |e| e.from);
                    while off < bytes.len() || handback_at == Some(off) {
                        // A slice never crosses the handback offset.
                        let limit = match handback_at {
                            Some(at) if at > off => at,
                            _ => bytes.len(),
                        };
                        let end = off
                            + ingest_chunk_width(
                                || {
                                    crate::metrics::input_pending()
                                        || ui_waiting.load(Ordering::Relaxed) != 0
                                },
                                limit - off,
                            );
                        // The pasteboard and Kitty medium reads this slice asks
                        // for, done here, before its hold: the engine's callbacks
                        // only take them, so a read that blocks holds this reader,
                        // never the lock every window's redraw takes. The slice
                        // ends where their memory budget does; dropped once it is
                        // fed.
                        let fetched = prefetch.reads(&term, &bytes[off..end]);
                        let end = off + fetched.covered();
                        // Liveness is decided before the term lock: the probe is a
                        // `kill(pgid, 0)` plus, for a reaped leader, a member listing
                        // for `e.from`, and one `kill(-pgid, 0)` for any other owner
                        // (`FgRole`). Under the lock it would stall render and input
                        // at every command completion. `fg_owners` is reader-local
                        // and current here.
                        //
                        // The master is named only while a sink clone keeps it
                        // open, `held` across the probe: after a tab close the
                        // gather may drop the last clone while batches it queued
                        // are still ingested here, and a closed number can
                        // already name another descriptor. `-1` sends the Linux
                        // member walk to its `/proc` fallback.
                        let verdicts = match (handback_at, edge) {
                            (Some(at), Some(e)) if at == off => {
                                let held = probe_sink.upgrade();
                                let master = if held.is_some() { master } else { -1 };
                                Some(fg_owners.probe_edge(e.from, e.to, |pgid, role| {
                                    fg_gone(master, pgid, role)
                                }))
                            }
                            _ => None,
                        };
                        {
                            let mut t = term_lock(&term);
                            cuts.observe(emitted, &t);
                            // Temporal spine: the engine's resize ordinal and geometry
                            // before this chunk. An external resize (window resize or the
                            // `resize` verb) runs under term_lock between chunks and is
                            // journalled, so the next read finds each one. `process()`
                            // never resizes this engine (DECCOLM is flag-only, XTWINOPS 8
                            // goes through the host), so there is no in-band resize to
                            // reconcile. Read only when recording.
                            let spine_pre = temporal_tx
                                .as_ref()
                                .map(|_| crate::temporal::SpinePre::take(&t));
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
                            if let (Some(e), Some(v)) = (edge, verdicts) {
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
                                        spine_mark.record(tx, pre, &t, &h.bytes);
                                    }
                                    handback = Some(h);
                                }
                            }
                            t.process(&bytes[off..end]);
                            prefetch.held(&t);
                            emitted += end - off;
                            // A mode this slice armed again belongs to its holder too
                            // (`take_evidence_asserted`), so a stale owner hides no death.
                            fg_owners.observe(
                                slice_holder,
                                t.program_evidence(),
                                t.take_evidence_asserted(),
                            );
                            if let Some(r) = t.take_response() {
                                match &mut acc {
                                    Some(a) => a.extend_from_slice(&r),
                                    None => acc = Some(r),
                                }
                            }
                            // THRU-5: read the deferred-compression backlog under the
                            // lock we already hold (cheap), to decide below whether to
                            // wake the compression worker. Overwritten each chunk; the
                            // last value reflects the whole burst.
                            backlog = t.lazy_backlog_len();
                            // Record this chunk (input plus every preceding external
                            // resize) under the term_lock, so spine order equals engine
                            // order. If a resize is dropped on a full queue, skip this
                            // chunk's RawIn and leave the watermark so the next chunk
                            // retries: output is never recorded at a geometry the spine
                            // does not reflect. Per-chunk `Arc`; the whole-burst `Arc`
                            // below feeds the cast and byte taps.
                            if let (Some(tx), Some(pre)) = (&temporal_tx, spine_pre)
                                && end > off
                            {
                                spine_mark.record(tx, pre, &t, &bytes[off..end]);
                            }
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
                    acc
                };
                // Report the handback (term lock released; the timeline is a
                // strict leaf, taken here with no other lock held).
                if let (Some(h), Some(e)) = (&handback, edge) {
                    let payload = {
                        let mut tl = timeline.lock().unwrap_or_else(|p| p.into_inner());
                        let payload = crate::foreground_handback::restored_payload(
                            e,
                            tl.program_of(e.from),
                            &h.reverted,
                            h.bytes.len(),
                        );
                        tl.record("modes-restored", payload.clone());
                        payload
                    };
                    aterm_log::info!("session {id}: foreground handback: {payload}");
                }
                // Report a manual reset the same way (term lock released, the
                // timeline a strict leaf), then answer whoever asked.
                if let Some((req, report, _)) = &manual_run {
                    crate::manual_reset::record(id, &timeline, req.source, report);
                    if let Some(reply) = &req.reply {
                        let _ = reply.try_send(report.clone());
                    }
                }
                // If this burst pushed the deferred backlog past the signal point,
                // wake the compression worker. `try_send` on the capacity-1 channel:
                // a queued token means a drain is pending. `None`: no worker, the
                // offload is inactive and the reader already drained inline.
                if backlog >= COMPRESS_SIGNAL_AT
                    && let Some(tx) = &compress_tx
                {
                    let _ = tx.try_send(());
                }
                if let Some(resp) = response {
                    // ONE shared allocation for the reply, split between the spine
                    // record and the reply-writer hand-off (refcount bumps instead
                    // of byte copies). Record BEFORE handing it to the peer; not
                    // re-emitted on replay (the recorder's contract).
                    let resp: std::sync::Arc<[u8]> = resp.into();
                    if let Some(tx) = &temporal_tx {
                        // Bounded `try_send` (see the RawIn tap above): drop on a full queue.
                        let _ = tx.try_send(crate::temporal::TemporalMsg::Reply(resp.clone()));
                    }
                    // Hand the reply to the writer thread, never write it inline (a
                    // blocking write would deadlock the session; see
                    // `spawn_reply_writer`). `submit` never blocks; on a full lane it
                    // drops the reply.
                    let _ = reply_tx.submit(resp);
                }
                // Coalesce wakes: post `Wake::Output` only on the latch's clear-to-
                // armed edge ([`gated_output_wake`]). The arm timestamp is read here,
                // not the burst's arrival stamp, so the latch's expiry measures only
                // how long a posted wake has gone unhandled.
                //
                // The read after the post is the latch's sent stamp: the watchdog
                // presumes a wake lost only once a user-event drain began after it
                // ([`judge_output_wake`]).
                //
                // An armed latch is re-posted only when its wake was stranded while
                // the loop slept, never one queued behind a long frame.
                let woke = Instant::now();
                let now = (woke.saturating_duration_since(lat_epoch).as_nanos() as u64).max(1);
                let gate = gated_output_wake(
                    &output_wake_pending,
                    now,
                    loop_idle.since_ns(),
                    || {
                        post(Wake::Output {
                            session: id,
                            window,
                        })
                    },
                    || lat_epoch.elapsed().as_nanos() as u64,
                );
                if gate == WakeGate::Reposted {
                    crate::metrics::note_output_wake_repost();
                    aterm_log::info!(
                        "session {id}: re-posted an output wake stranded while the event \
                         loop slept (posted after its last turn, none since, older than {} ms)",
                        WAKE_LATCH_EXPIRY_NS / 1_000_000
                    );
                }
                // Cast and byte-fanout taps: program output only; `take_response()`
                // replies are the terminal's own bytes and are not `"o"` events. One
                // heap copy of the burst, shared via `Arc`. The temporal RawIn is
                // sent per chunk under the term_lock above, not here.
                //
                // After the wake, since the copy is a full allocation and memcpy no
                // pixel depends on. The recorder is always on (`cast` / `cast
                // frames` serialize whatever the session produced, with no arming
                // step), so it has no kill switch. Order is kept by the burst's
                // `cuts`, not by send time.
                // A handback's synthesized bytes ride the taps at the cut, and a
                // manual reset's bytes lead the burst.
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
                    _ => std::sync::Arc::from(&buf[..]),
                };
                // `try_send`: the cast queue is bounded (`CAST_QUEUE_CAP`), so a
                // stalled writer drops this burst instead of blocking the reader.
                // Drops are counted in the recording header (`aterm_dropped`) and
                // `cast drift`'s `dropped=`. An empty burst reaches neither tap.
                // `emitted` walked the same bytes in the same order.
                debug_assert_eq!(emitted, burst.len(), "cast cuts address the spliced burst");
                if !burst.is_empty() {
                    let stamped = crate::cast::CastBurst::new(
                        burst.clone(),
                        cuts,
                        arrived
                            .unwrap_or(woke)
                            .saturating_duration_since(cast_epoch),
                    );
                    if cast_tx.try_send(stamped).is_err() {
                        cast_drops.note(burst.len());
                    }
                    // Live byte fan-out (Item 2): tee the SAME burst to any `bytes`
                    // subscribers — one refcount bump, never blocks the reader.
                    byte_fanout.tee(&burst);
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
                let r = match aterm_pty::read_or_wake(master, &mut buf, wake_rd, -1) {
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
                    // `Exited` is never produced here: the child's exit EOFs the read.
                    aterm_pty::ReadOutcome::Eof | aterm_pty::ReadOutcome::Exited => {
                        // The PTY closed on its OWN (shell/`-e` exited). Route an Exit for THIS
                        // session; the main thread closes only this tab and exits the app only
                        // if it was the last (honoring `--hold`).
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
                        let filled = aterm_pty::drain_more(master, &mut buf, n);
                        let output_activity_now =
                            u64::try_from(lat_epoch.elapsed().as_nanos()).unwrap_or(u64::MAX);
                        latest_output_activity_ns
                            .store(output_activity_now.max(1), Ordering::Release);
                        filled as isize
                    }
                };
                // ConPTY has no foreground process group: no handback.
                ingest(&buf[..r as usize], None, 0);
            }
            // Unix parse loop: batches from the gather stage. Data batches have
            // left the kernel queue, so they are always processed; the gather
            // enforces stop before its next read, and the channel always ends with
            // Eof/Wake or close, so `recv()` needs no timeout.
            #[cfg(unix)]
            loop {
                match filled_rx.recv() {
                    Ok(GatherMsg::Started) => {
                        let _ = post(Wake::Ready {
                            session: id,
                            window,
                        });
                    }
                    Ok(GatherMsg::Data(batch, len, edge, holder)) => {
                        ingest(&batch[..len], edge, holder);
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
                        ingest(&[], None, 0);
                    }
                }
            }
            // This stage takes no more resets: close the lane now rather than
            // when the gather's guard drops, so a request that races the end
            // runs directly instead of waiting out its timeout.
            #[cfg(unix)]
            reset_lane.close_if(lane_generation);
            // Windows: this thread owns the wake pipe's READ end — close it on
            // exit (the write end is owned + closed by `Session::drop`). No-op
            // for the `-1` sentinel. On unix the GATHER owns + closes it.
            #[cfg(windows)]
            aterm_pty::close_fd(wake_rd);
        })
        .map_err(|error| format!("spawn PTY reader: {error}"))?;
    #[cfg(unix)]
    start_gather()?;
    Ok(parse)
}

/// The term-lock hold width for one ingest slice of a PTY burst: fine (8 KiB)
/// while a keystroke is waiting to echo (`pending`, the lock-free
/// [`crate::metrics::input_pending`] signal), otherwise a wider but capped slice
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

#[cfg(test)]
mod ingest_chunk_tests {
    use super::ingest_chunk_width;

    /// The interactivity contract: a pending keystroke caps every hold at the
    /// fine slice; an idle input path takes a WIDER but still BOUNDED hold (so a
    /// key arriving mid-burst waits at most one capped process, not a whole
    /// 64 KiB batch); the tail slice never overruns what is left.
    #[test]
    fn chunk_width_fine_when_pending_capped_when_idle() {
        assert_eq!(ingest_chunk_width(|| true, 64 * 1024), 8 * 1024);
        assert_eq!(ingest_chunk_width(|| true, 3 * 1024), 3 * 1024); // tail < chunk
        assert_eq!(ingest_chunk_width(|| false, 64 * 1024), 16 * 1024);
        assert_eq!(ingest_chunk_width(|| false, 12 * 1024), 12 * 1024); // tail < cap
        assert_eq!(ingest_chunk_width(|| false, 1), 1);
        // The idle hold is always at least as wide as the pending one — the
        // interactivity signal must never make the reader slower.
        assert!(ingest_chunk_width(|| false, 64 * 1024) >= ingest_chunk_width(|| true, 64 * 1024));
        // Degenerate zero-remainder never yields a zero-progress loop upstream:
        // the reader's loop condition (`off < bytes.len()`) is what prevents a
        // zero-width call; document the pure fn's behavior anyway.
        assert_eq!(ingest_chunk_width(|| true, 0), 0);
        assert_eq!(ingest_chunk_width(|| false, 0), 0);
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
/// Clear: the reader has armed and is still posting. Nanoseconds since the
/// epoch reach bit 63 after 292 years, so the bit never collides with a time.
pub(crate) const WAKE_LATCH_SENT: u64 = 1 << 63;

/// The event loop's turn state as the PTY readers see it: the `lat_epoch`
/// stamp of the `about_to_wait` that last put the loop to sleep, or `0` while a
/// turn is running (from `new_events` or any `window_event`; a macOS `drawRect:`
/// can run outside the `new_events`/`about_to_wait` bracket) and before the
/// first turn ends.
///
/// Used by [`gated_output_wake`] to decide whether a posted `Wake::Output` was
/// stranded together with its loop wake-up. The watchdog
/// ([`OutputWakeWatch::pass`]) only sees latches an `about_to_wait` has seen, so
/// a wake posted after the loop went idle whose wake-up never arrived has no
/// recheck. A latch stamped after `since_ns()`, with the loop still idle and
/// older than the expiry, is such a wake; a wake queued behind a long frame
/// never is (the loop is not idle).
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
}

/// Whether an armed latch `word` is a wake stranded with its loop wake-up:
/// sent, stamped after the loop last went idle (`idle_since_ns`, `0` = a turn is
/// running), and older than [`WAKE_LATCH_EXPIRY_NS`]. See [`LoopIdle`]. A wake
/// possibly queued behind a long frame, one an `about_to_wait` already saw, and
/// an unsent arm never qualify.
pub(crate) fn wake_stranded(word: u64, idle_since_ns: u64, now_ns: u64) -> bool {
    let at = word & !WAKE_LATCH_SENT;
    word & WAKE_LATCH_SENT != 0
        && idle_since_ns != 0
        && idle_since_ns < at
        && now_ns.saturating_sub(at) > WAKE_LATCH_EXPIRY_NS
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
/// lets the watchdog tell a lost wake from a late one.
///
/// The reader never re-sends a wake that may be queued: an armed latch
/// coalesces however old, unless [`wake_stranded`] (posted after the loop last
/// went idle, no turn since). A wake the loop has turned past is the watchdog's
/// to heal ([`OutputWakeWatch::pass`]); a stranded one is re-posted here (by CAS,
/// not counted as a heal) while output streams. A wake stranded on the last
/// chunk of a burst waits for the loop's next unrelated turn.
fn gated_output_wake(
    flag: &AtomicU64,
    now_ns: u64,
    idle_since_ns: u64,
    send: impl FnOnce() -> bool,
    sent_ns: impl FnOnce() -> u64,
) -> WakeGate {
    let word = flag.load(Ordering::Relaxed);
    let (from, posted) = if word == 0 {
        (0, WakeGate::Posted)
    } else if wake_stranded(word, idle_since_ns, now_ns) {
        (word, WakeGate::Reposted)
    } else {
        return WakeGate::Coalesced; // armed: this chunk rides the wake already posted
    };
    let arm = (now_ns & !WAKE_LATCH_SENT).max(1);
    // CAS from the observed word so concurrent readers arm exactly once, and a
    // handler clear (or a watchdog heal) that lands first stands.
    if flag
        .compare_exchange(from, arm, Ordering::Relaxed, Ordering::Relaxed)
        .is_err()
    {
        return WakeGate::Coalesced;
    }
    if send() {
        // CAS from the arm: a handler that already consumed the event cleared
        // the latch, and that clear must stand.
        let stamp = (sent_ns() & !WAKE_LATCH_SENT).max(arm) | WAKE_LATCH_SENT;
        let _ = flag.compare_exchange(arm, stamp, Ordering::Relaxed, Ordering::Relaxed);
        posted
    } else {
        let _ = flag.compare_exchange(arm, 0, Ordering::Relaxed, Ordering::Relaxed);
        WakeGate::Coalesced
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
/// (`metrics::record_deadline`) on every long frame.
pub(crate) fn judge_output_wake(
    word: u64,
    drained_through_ns: u64,
    now_ns: u64,
) -> OutputWakeVerdict {
    if word == 0 {
        return OutputWakeVerdict::Clear;
    }
    let at = word & !WAKE_LATCH_SENT;
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
                                now_ns.saturating_sub(word & !WAKE_LATCH_SENT) / 1_000_000,
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
    /// cross-session reads are zero-copy. Called at the spawn seams (`open_tab`
    /// and the startup `session0`); deregistered at `close_tab_at`.
    ///
    /// The handle is registered `Spawning`: engine, master and sink exist (input
    /// and reads are safe; early bytes buffer in the kernel), but the reader has
    /// not confirmed its first iteration. The reader flips it to `Alive` by
    /// posting `Wake::Ready` ([`spawn_pty_reader`]), handled via
    /// [`session_store::SessionStore::mark_alive`]. There is no artificial delay,
    /// and a slow shell stays `Spawning` and addressable without blocking the GUI.
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
            // The identity label rides the handle (a field, not a side table):
            // every roster reads handles, and the next handoff record projects it.
            identity: session.identity.as_deref().map(Arc::from),
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
        // instance socket) so an `@<sid>` arriving at ANOTHER same-uid instance
        // can be forwarded here. No-op until/unless the control socket is bound.
        crate::proxy::publish_session(&session.ctx.self_id, &session.ctx.nonce);
    }
}

/// The directory holding aterm's co-located CLI tools (`aterm-ctl`, `atpkg`)
/// when running from a macOS .app bundle (`Contents/MacOS`), put on child
/// sessions' PATH so the bundled Help's workflows run with no install step.
/// `None` outside a bundle.
pub(crate) fn bundle_dir(exe: Option<&std::path::Path>) -> Option<String> {
    let dir = exe?.parent()?;
    // Component-wise suffix match: only a real `<Name>.app/Contents/MacOS` layout
    // qualifies (the shape codesign seals), so this is inert for dev/CI binaries.
    if !dir.ends_with("Contents/MacOS") {
        return None;
    }
    dir.to_str().map(str::to_owned)
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
///    it first. Order matters, not just presence.
/// 2. `agents_dir`: `<prefix>/agents/`, holding only the shims of the agent
///    programs aterm manages (`atpkg::stub::AGENT_PROGRAMS`),
///    with the same move-to-front rule, so it outranks `~/.local/bin` and
///    Homebrew copies: for these names aterm's copy is what runs (owner,
///    2026-09-10; `docs/DESIGN-which-copy-runs-2026-08-27.md`). Passed only
///    when the directory exists.
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
    // two name sets are disjoint (upstream Rust names vs. the agent programs), so
    // which of the two is first is immaterial; that BOTH precede everything else is
    // the point.
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
    static PATH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    PATH.get_or_init(atpkg_child_path)
}

/// The PATH the co-located atpkg children run with. A Finder-launched app
/// inherits launchd's minimal PATH, so `atpkg seed`/`atpkg update` could not see
/// foreign copies like `~/.local/bin/claude` and would disagree with a terminal.
/// The children get the login shell's PATH: one `$SHELL -l -i -c 'printf %s
/// "$PATH"'` (zsh/bash/fish; `/bin/zsh` otherwise), bounded by
/// [`LOGIN_PATH_BUDGET`], falling back to the process PATH plus `~/.local/bin`
/// and `/opt/homebrew/bin` when the shell does not answer.
pub(crate) fn atpkg_child_path() -> String {
    login_shell_path().unwrap_or_else(|| {
        // The managed dirs the shell hook would place (`agents/` first, `bin/`
        // last), so a fallback PATH cannot report a false shadowed row for a
        // `claude` the managed twin outranks.
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
    })
}

/// How long [`atpkg_child_path`] waits for the login shell before falling back: an
/// rc file that blocks (a `read`, an `ssh-add` prompt) must not stall provisioning.
const LOGIN_PATH_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

/// How long [`login_shell_path`] keeps collecting after the shell has answered: the
/// PATH line is already in the pipe then, so this only covers the reader's hop.
const LOGIN_PATH_GRACE: std::time::Duration = std::time::Duration::from_millis(50);

/// The user's login-shell `PATH`, or `None` when the shell did not answer in time /
/// printed nothing that looks like one.
///
/// Always `None` on Windows, where a login shell is a POSIX concept: the `PATH` a
/// Git Bash or MSYS `bash` prints is its own `:`-separated spelling
/// (`/c/Users/…:/usr/bin`), which no native child resolves programs against, so
/// [`atpkg_child_path`] takes [`fallback_child_path`]. Asking anyway started that
/// `bash` from the window, which has no console, and the console image opened a
/// visible console window for its whole rc chain (the 2026-09-30 sweep;
/// `aterm_uds::spawn` has the measurement).
fn login_shell_path() -> Option<String> {
    if cfg!(windows) {
        return None;
    }
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|s| {
            matches!(
                std::path::Path::new(s).file_name().and_then(|n| n.to_str()),
                Some("zsh" | "bash" | "fish")
            )
        })
        .unwrap_or_else(|| "/bin/zsh".to_string());
    login_shell_path_with(
        &shell,
        &["-l", "-i", "-c", "printf '%s\\n' \"$PATH\""],
        LOGIN_PATH_BUDGET,
    )
}

// How many times THIS thread has entered [`login_shell_path_with`]. The Windows
// test proves `login_shell_path` asks no shell by this count, which means
// something on a box with no bash at all, where a failed spawn would also
// answer `None`.
#[cfg(test)]
thread_local! {
    static LOGIN_SHELL_ASKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// [`login_shell_path`]'s body over an explicit program, argv and budget, so the
/// budget is testable against a plain `/bin/sh`.
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
    // Its own SESSION, so a timeout kills what the rc file is blocked in (a
    // `sleep`, a password helper) along with the shell: those grandchildren hold
    // the stdout pipe too, and a session leader's pid is also its group's id, so
    // `killpg(child.id())` below reaches them. A session and not just a group
    // (2026-09-12): when this GUI has a controlling terminal and sits in its
    // foreground (`aterm --window` typed at a shell, `targo run`), `zsh -i` in a
    // mere background group opens `/dev/tty`, tries to take the foreground, takes
    // SIGTTOU and sits STOPPED until the budget runs out. A new session has no
    // controlling tty at all, so there is nothing for the shell to take. (A
    // spawn attribute: `POSIX_SPAWN_SETSID` on Darwin, no code of ours between a
    // fork and an exec.)
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
                // BEFORE the leader is reaped (its pid names the group until then).
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

/// The PATH line out of a login shell's output: the LAST non-empty line that starts
/// with `/` (rc-file banners print before it; a PATH may contain spaces — the managed
/// store lives under `Application Support`). Pure, so the shape is testable.
pub(crate) fn pick_login_path(output: &str) -> Option<String> {
    output
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| line.starts_with('/') && line.contains(':'))
        .map(str::to_string)
}

/// The fallback child PATH: the process PATH (launchd's four dirs, for a Finder
/// launch) plus the two places a foreign `claude`/`codex` measurably lives —
/// `~/.local/bin` (Anthropic's native installer) and `/opt/homebrew/bin` (casks) —
/// and `/usr/local/bin`, each appended once. Pure.
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
    extra.push("/opt/homebrew/bin".to_string());
    extra.push("/usr/local/bin".to_string());
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

    /// The agents dir rides beside the reroute dir at the front, ahead of the
    /// bundle dir and every inherited entry (including `~/.local/bin` and
    /// `/opt/homebrew/bin`), and is moved, not duplicated, when already present.
    #[test]
    fn the_agents_dir_is_front_inserted_beside_the_reroute_dir() {
        let inherited = path(&[
            "/Users//u/.local/bin",
            "/opt/homebrew/bin",
            "/usr/bin",
            AGENTS,
        ]);
        let (_, value) = reroute_path_env(
            Some(REROUTE),
            Some(AGENTS),
            Some(BUNDLE),
            Some(&inherited),
            None,
        )
        .expect("injects");
        assert_eq!(
            value,
            path(&[
                REROUTE,
                AGENTS,
                BUNDLE,
                "/Users//u/.local/bin",
                "/opt/homebrew/bin",
                "/usr/bin"
            ]),
            "reroute, agents, bundle, then the user's PATH with the agents dir moved"
        );
        assert_eq!(
            value.matches(AGENTS).count(),
            1,
            "moved to the front, never duplicated"
        );
        // Without a reroute dir the agents dir alone is still an injection…
        let (_, value) =
            reroute_path_env(None, Some(AGENTS), None, Some("/usr/bin"), None).expect("injects");
        assert_eq!(value, path(&[AGENTS, "/usr/bin"]));
        // …and with neither, nothing changes.
        assert_eq!(
            reroute_path_env(None, None, None, Some("/usr/bin"), None),
            None
        );
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
        // A symlink at `agents/` — even one that resolves to a real directory — and a
        // regular file there are REFUSED, the front door's rule: before this the window
        // warned and handed the linked directory all the same (`Path::is_dir` follows).
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
        // And it is what the seam front-inserts, ahead of the foreign homes.
        // (Unix-only: the fixture PATH and the `:` join are a Unix login
        // shell's; Windows joins with `;`.)
        #[cfg(unix)]
        {
            let (_, value) = reroute_path_env(
                None,
                Some(&dir),
                None,
                Some("/Users//u/.local/bin:/opt/homebrew/bin"),
                None,
            )
            .expect("injects");
            assert!(value.starts_with(&format!("{dir}:")), "{value}");
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
        assert_eq!(
            fallback_child_path(Some("/usr/bin:/bin:/usr/sbin:/sbin"), Some(home), None),
            path(&[
                "/usr/bin",
                "/bin",
                "/usr/sbin",
                "/sbin",
                "/Users//u/.local/bin",
                "/opt/homebrew/bin",
                "/usr/local/bin"
            ]),
            "launchd's four, then the foreign homes"
        );
        assert_eq!(
            fallback_child_path(Some("/opt/homebrew/bin:/usr/bin"), None, None),
            path(&["/opt/homebrew/bin", "/usr/bin", "/usr/local/bin"]),
            "already-present dirs are not repeated; no home, no ~/.local/bin"
        );
        assert_eq!(
            fallback_child_path(None, Some(home), None),
            path(&["/Users//u/.local/bin", "/opt/homebrew/bin", "/usr/local/bin"])
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
            path(&[
                agents,
                "/usr/bin",
                "/bin",
                "/Users//u/.local/bin",
                "/opt/homebrew/bin",
                "/usr/local/bin",
                bin
            ]),
            "agents/ moved to the front, bin/ appended"
        );
    }

    /// NO LOGIN SHELL IS ASKED ON WINDOWS (2026-09-30). The window has no
    /// console, so a console `bash` it starts puts a console window on screen
    /// for its whole rc chain, and the `PATH` a Git Bash prints
    /// (`/c/Users/…:/usr/bin`) is no Windows search path anyway. `SHELL` names a
    /// bash, as Git Bash and MSYS2 export it.
    ///
    /// The proof is a COUNT, not the `None` alone: a bash that is not there also
    /// answers `None`, by a spawn that failed. `login_shell_path` must not enter
    /// [`super::login_shell_path_with`] at all, counted on this thread
    /// (`LOGIN_SHELL_ASKS`), so the check means the same on a box with no bash.
    /// The control shows a direct ask IS counted. Where Git for Windows is
    /// installed, `SHELL` names that real bash, and the control also shows it
    /// answers a `PATH` when asked directly.
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

    /// THE RC FILES RUN BELOW THE TYPING BAND (2026-09-15). The lookup runs a
    /// login shell's whole rc chain for an `atpkg` pass, on a `Background` worker —
    /// and started it at pri 31, the band of the program being typed into, because
    /// a thread's QoS does not cross the spawn. The probe shell now reports its own
    /// priority as its "PATH".
    ///
    /// The verdict is the reported band, not the timing; the budget only ends a
    /// lookup that never answers, so it is generous. A probe still running at the
    /// deadline answers `None`, which the `expect` reports as a missing answer.
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
    /// The bound is the job's own lifetime, so it separates the two outcomes by
    /// cause, not by a guess about load. The job starts after the clock does and
    /// is killed only once the lookup has returned, so a lookup that waits for EOF
    /// on the pipe the job holds returns after `JOB` on any machine, and one that
    /// returns before `JOB` did not wait for the job. The budget sits well inside
    /// `JOB` and is a hang detector, never a latency budget, because the shell
    /// runs behind the utility clamp (`qos::command`), below every default-band
    /// build beside the suite.
    ///
    /// The answer carries the job's pid so the test can end the job. A job left
    /// to run out its two minutes would keep every unflagged fd this process had
    /// open at the fork, and a sibling test's closed pipe peer would read as alive
    /// for that long.
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
    /// The lookup runs in a re-exec of this test binary (a stand-in, judged by
    /// its own `1 passed`) that leads a session on a pty of its own, so it has a
    /// controlling tty in the foreground, and the tty stays out of the parallel
    /// test process.
    ///
    /// THE ANSWER IS THE CHECK, NOT A STOPWATCH (2026-09-24). A shell stopped by
    /// SIGTTOU never exits, and the lookup answers `None` for any shell that has
    /// not exited by its deadline, so the regression fails the `Some` assertion
    /// at every budget. A healthy shell answers late only on a busy machine: since
    /// 1513cda06 the lookup starts it under `taskpolicy -c utility`
    /// (`qos::command`), below every default-band build on the machine, and at a
    /// load average near 20 a healthy answer took 1.34 s. The `< 1 s` bound this
    /// test carried was set before that clamp, failed that run, and separated
    /// nothing the `Some` assertion does not. The budget is only the backstop
    /// that ends a regressed run, so it is generous, and it stays well inside the
    /// stand-in's hang detector so the child reports its own `None`.
    #[test]
    #[cfg(unix)]
    fn login_path_answers_under_a_foreground_controlling_tty() {
        const CHILD: &str = "ATERM_TEST_LOGIN_PATH_UNDER_A_PTY";
        const ZSH: &str = "/bin/zsh";
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
            let started = std::time::Instant::now();
            let path = super::login_shell_path_with(
                ZSH,
                &["-f", "-i", "-c", "printf '/a:/b\\n'"],
                aterm_spec::wait::MUST_HAPPEN,
            );
            let elapsed = started.elapsed();
            assert_eq!(path.as_deref(), Some("/a:/b"), "after {elapsed:?}");
            return;
        }
        if !std::path::Path::new(ZSH).exists() {
            eprintln!("skipped: no {ZSH} on this host");
            return;
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
        stand_in
            .command()
            .env(CHILD, "1")
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

    /// The reroute dir lands FIRST, ahead of the bundle dir and the inherited PATH.
    #[test]
    fn reroute_dir_is_first_then_bundle_then_inherited() {
        let got = reroute_path_env(
            Some(REROUTE),
            None,
            Some(BUNDLE),
            Some(&path(&["/usr/bin", "/bin"])),
            None,
        );
        assert_eq!(
            got,
            Some((
                "PATH".to_string(),
                path(&[REROUTE, BUNDLE, "/usr/bin", "/bin"])
            ))
        );
    }

    /// Move-to-front, not skip-if-present: a nested aterm inherits a PATH listing
    /// the reroute dir later (behind `~/.cargo/bin`); it must come out first,
    /// exactly once, and reapplying the seam changes nothing.
    #[test]
    fn a_later_occurrence_moves_to_the_front_without_duplicates() {
        let inherited = path(&[
            "/opt/homebrew/bin",
            "/Users//u/.cargo/bin",
            REROUTE,
            "/usr/bin",
            REROUTE,
        ]);
        let (_, value) =
            reroute_path_env(Some(REROUTE), None, None, Some(&inherited), None).expect("injects");
        assert_eq!(
            value,
            path(&[
                REROUTE,
                "/opt/homebrew/bin",
                "/Users//u/.cargo/bin",
                "/usr/bin"
            ])
        );
        assert_eq!(value.matches(REROUTE).count(), 1);
        let (_, again) =
            reroute_path_env(Some(REROUTE), None, None, Some(&value), None).expect("injects");
        assert_eq!(again, value, "idempotent under nesting");
    }

    /// The bundle dir composes behind the reroute dir and is never stacked when it
    /// is already reachable; without a reroute dir (Windows, `--no-reroute`) the pair
    /// is the old bundle-only contract, byte for byte.
    #[test]
    fn bundle_dir_composes_once() {
        let already = path(&[BUNDLE, "/usr/bin"]);
        let (_, value) = reroute_path_env(Some(REROUTE), None, Some(BUNDLE), Some(&already), None)
            .expect("injects");
        assert_eq!(value, path(&[REROUTE, BUNDLE, "/usr/bin"]));
        assert_eq!(
            reroute_path_env(None, None, Some(BUNDLE), Some("/usr/bin"), None),
            Some(("PATH".to_string(), path(&[BUNDLE, "/usr/bin"])))
        );
        assert_eq!(
            reroute_path_env(None, None, Some(BUNDLE), Some(&already), None),
            None
        );
    }

    /// No inherited PATH at all (odd launchd edge) or an empty one: the injected dirs
    /// alone, no dangling separator; nothing to inject ⇒ no pair; an empty entry
    /// ("here") inside the inherited PATH is the user's and survives.
    #[test]
    fn empty_or_absent_inherited_path_and_nothing_to_inject() {
        assert_eq!(
            reroute_path_env(Some(REROUTE), None, Some(BUNDLE), None, None).map(|p| p.1),
            Some(path(&[REROUTE, BUNDLE]))
        );
        assert_eq!(
            reroute_path_env(Some(REROUTE), None, None, Some(""), None).map(|p| p.1),
            Some(REROUTE.to_string())
        );
        assert_eq!(
            reroute_path_env(None, None, None, Some("/usr/bin"), None),
            None
        );
        assert_eq!(reroute_path_env(None, None, None, None, None), None);
        let (_, value) = reroute_path_env(
            Some(REROUTE),
            None,
            None,
            Some(&path(&["/usr/bin", "", "/bin"])),
            None,
        )
        .expect("injects");
        assert_eq!(value, path(&[REROUTE, "/usr/bin", "", "/bin"]));
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
    /// outside one (dev builds) and safe with no exe at all.
    #[test]
    fn bundle_dir_is_only_a_real_app_layout() {
        assert_eq!(
            bundle_dir(Some(Path::new(
                "/Applications/aterm.app/Contents/MacOS/aterm"
            )))
            .as_deref(),
            Some(BUNDLE)
        );
        assert_eq!(
            bundle_dir(Some(Path::new("/Users//u/aterm/target/release/aterm-gui"))),
            None
        );
        assert_eq!(bundle_dir(None), None);
    }
}

#[cfg(test)]
mod clipboard_coalesce_tests {
    use aterm_core::terminal::ClipboardSelection as Sel;
    use aterm_spec::derive::{Model, clipboard_mailbox_model};

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

    /// `'c'` (and the empty default) keep their established clipboard routing,
    /// with PRIMARY untouched; `"cp"` sets both.
    #[test]
    fn clipboard_and_default_targets_route_to_the_clipboard() {
        let w = route_osc52_write(&[Sel::Clipboard], "x", true);
        assert_eq!((w.clipboard.as_deref(), w.primary), (Some("x"), None));
        let w = route_osc52_write(&[], "x", true);
        assert_eq!(
            (w.clipboard.as_deref(), w.primary),
            (Some("x"), None),
            "an empty selection list is the xterm clipboard default"
        );
        let w = route_osc52_write(&[Sel::Clipboard, Sel::Primary], "x", true);
        assert_eq!(w.clipboard.as_deref(), Some("x"));
        assert_eq!(w.primary.as_deref(), Some("x"));
    }

    /// Off X11 there IS no PRIMARY: `'p'` falls back to the clipboard (the
    /// behaviour every macOS/Windows OSC 52 client has always relied on), and a
    /// `"cp"` set collapses to ONE clipboard write, not two.
    #[test]
    fn primary_falls_back_to_the_clipboard_where_primary_does_not_exist() {
        let w = route_osc52_write(&[Sel::Primary], "y", false);
        assert_eq!((w.clipboard.as_deref(), w.primary), (Some("y"), None));
        let w = route_osc52_write(&[Sel::Clipboard, Sel::Primary], "y", false);
        assert_eq!((w.clipboard.as_deref(), w.primary), (Some("y"), None));
    }

    /// `'p'` Sets write PRIMARY, so a `'p'` query reads PRIMARY, while `'c'` and
    /// the empty xterm default read the clipboard: set-then-query coherence.
    #[test]
    fn query_reads_the_slot_its_set_wrote_per_target() {
        // 'p' on X11: Set writes ONLY primary; the query reads Primary.
        let w = route_osc52_write(&[Sel::Primary], "yank", true);
        assert_eq!((w.clipboard, w.primary.as_deref()), (None, Some("yank")));
        assert_eq!(
            route_osc52_query(&[Sel::Primary], true),
            QuerySource::Primary
        );
        // 'c' and the empty default: Set writes the clipboard; so reads the query.
        assert_eq!(
            route_osc52_query(&[Sel::Clipboard], true),
            QuerySource::Clipboard
        );
        assert_eq!(route_osc52_query(&[], true), QuerySource::Clipboard);
        // "cp": Set wrote BOTH slots, the one response keeps the established
        // CLIPBOARD priority — still a slot the same Set stored.
        assert_eq!(
            route_osc52_query(&[Sel::Clipboard, Sel::Primary], true),
            QuerySource::Clipboard
        );
        // Off X11 'p' folds into the clipboard on BOTH sides of the round-trip.
        let w = route_osc52_write(&[Sel::Primary], "y", false);
        assert_eq!((w.clipboard.as_deref(), w.primary), (Some("y"), None));
        assert_eq!(
            route_osc52_query(&[Sel::Primary], false),
            QuerySource::Clipboard
        );
    }

    /// End-to-end on the stubbed slots (Linux): with DIFFERENT text stored in
    /// the CLIPBOARD and PRIMARY own-slots, the routed query reader returns the
    /// PRIMARY text for 'p' and the CLIPBOARD text for 'c' — the exact
    /// confusion of the finding ('p' answered from the CLIPBOARD slot).
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

    /// Mixed selection writes share one immutable payload allocation instead
    /// of copying a potentially large OSC 52 body once per destination.
    #[test]
    fn mixed_target_write_shares_its_payload() {
        let write = route_osc52_write(&[Sel::Clipboard, Sel::Primary], "shared", true);
        let clipboard = write.clipboard.expect("clipboard payload");
        let primary = write.primary.expect("primary payload");
        assert!(std::sync::Arc::ptr_eq(&clipboard, &primary));
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
/// spawned under ([`super::host_reach`], ruling 2026-10-10): its OSC 52 and
/// its desktop notifications here, its Kitty file mediums in
/// `kitty_transfer_tests`.
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
            aterm_types::Appearance::Dark,
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

    /// An OSC 52 query's pasteboard read is never made inside `process()`,
    /// which runs under the terminal lock (2026-10-14): a read that blocks (a
    /// Universal Clipboard fetch, a busy provider) held the lock every window's
    /// redraw takes. The reader reads before its hold, and the query is
    /// answered from that read, in its place among the burst's replies; fed
    /// with no read made first, as a hold alone feeds it, it reads nothing and
    /// is unanswered. (Linux answers from aterm's own selection, a read that
    /// never blocks, so it still reads in the hold.)
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

    /// A program in a Containment tab neither sets nor reads the clipboard
    /// through OSC 52, even after a later grant of the query (a config
    /// reload); a program in any other mode's tab still does both, which is
    /// what makes the refusal mean something.
    #[test]
    fn a_contained_session_reaches_no_clipboard() {
        let answer = b"\x1b]52;c;b3duZXItc2VjcmV0\x07".to_vec();
        for mode in [Cm::User, Cm::Master, Cm::Safety] {
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
        for mode in [Cm::User, Cm::Master, Cm::Safety, Cm::Containment] {
            let term = Arc::new(Mutex::new(new_live_terminal(
                24,
                80,
                Some(&notifications_allowed()),
                aterm_types::Appearance::Dark,
                None,
            )));
            let (tx, notices) = sync_channel(8);
            configure_notifications(&term, host_reach(mode), &tx, 1);
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
mod output_wake_coalesce_tests {
    use super::{
        LoopIdle, OutputWakeVerdict, OutputWakeWatch, WAKE_LATCH_EXPIRY_NS, WAKE_LATCH_SENT,
        WakeGate, gated_output_wake, judge_output_wake, wake_stranded,
    };
    use std::sync::atomic::{AtomicU64, Ordering};

    const MS: u64 = 1_000_000;

    /// A failed send (headless: no event loop) must RESET the latch — an arm
    /// left behind would suppress wakes until the watchdog proved it lost.
    #[test]
    fn failed_send_resets_the_flag() {
        let flag = AtomicU64::new(0);
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        drop(rx); // no event loop: every send errors
        let gate = gated_output_wake(&flag, 1, 0, || tx.send(()).is_ok(), || 2);
        assert_eq!(gate, WakeGate::Coalesced, "nothing was posted");
        assert_eq!(
            flag.load(Ordering::Relaxed),
            0,
            "failed send must not leave the latch armed"
        );
        // A later live receiver is still woken.
        let (tx2, rx2) = std::sync::mpsc::channel::<()>();
        let gate = gated_output_wake(&flag, 3, 0, || tx2.send(()).is_ok(), || 4);
        assert_eq!(gate, WakeGate::Posted);
        assert_eq!(rx2.try_iter().count(), 1);
    }

    /// The arm is two-phase: while `send` runs the word is the bare arm stamp
    /// (unsent), and after it returns the post-send stamp with
    /// [`WAKE_LATCH_SENT`]. An armed latch coalesces however old while a turn
    /// runs and once an `about_to_wait` has passed the post: the reader never
    /// duplicates a wake that may be queued.
    #[test]
    fn the_arm_is_stamped_after_the_post_and_a_queued_wake_is_never_resent() {
        let flag = AtomicU64::new(0);
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let mut during = 0;
        gated_output_wake(
            &flag,
            10 * MS,
            0,
            || {
                during = flag.load(Ordering::Relaxed);
                tx.send(()).is_ok()
            },
            || 11 * MS,
        );
        assert_eq!(during, 10 * MS, "unsent while the post is in progress");
        assert_eq!(flag.load(Ordering::Relaxed), (11 * MS) | WAKE_LATCH_SENT);
        // A turn is running (idle stamp 0), or the loop went idle AFTER the
        // post (an `about_to_wait` saw it): never re-sent, whatever the age.
        for idle_since in [0, 11 * MS, 12 * MS] {
            for age in [1, WAKE_LATCH_EXPIRY_NS + 1, 10 * WAKE_LATCH_EXPIRY_NS] {
                let gate = gated_output_wake(
                    &flag,
                    11 * MS + age,
                    idle_since,
                    || tx.send(()).is_ok(),
                    || u64::MAX,
                );
                assert_eq!(gate, WakeGate::Coalesced, "idle {idle_since} age {age}");
            }
        }
        assert_eq!(
            rx.try_iter().count(),
            1,
            "one wake per arm, whatever its age"
        );
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
    /// atomically with the arm, so every armed latch is SENT. Its stamp sits
    /// just BEFORE the tick when a `TurnEnd` saw it (the post preceded that
    /// pass, whose stamp is `drained`), and just AFTER it otherwise (posted
    /// after the last pass): the model's tick ties resolved as the real
    /// clocks order them.
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
    /// `aterm_spec::derive::damage_to_present_model()` reachable under `Buggy = 0`,
    /// the real [`gated_output_wake`], fed the projected latch word and loop idle
    /// stamp, posts iff `DamageFresh` is enabled, re-posts iff `DamageRepost` is,
    /// re-stamps the latch to the model successor's word, and otherwise coalesces.
    ///
    /// Negative controls: a reader that re-sends on an arm older than the expiry
    /// disagrees with the model on a wake queued behind a long frame; a reader
    /// that never re-sends disagrees on a wake stranded with its loop wake-up.
    #[test]
    fn latch_conforms_to_damage_to_present_model() {
        let m = aterm_spec::derive::damage_to_present_model();
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
        let m = aterm_spec::derive::damage_to_present_model();
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
                // wake LATER, and it heals a queued wake — the false heal.
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
        // The shape the pre-fix watchdog condemned: armed, and older than the expiry.
        assert!(end - (t0 + 10 * MS) > WAKE_LATCH_EXPIRY_NS);
        loop_idle.went_idle(end);
        // A chunk between this `about_to_wait` and the next turn: the pass saw
        // the wake, so it is not stranded.
        let gate = gated_output_wake(
            &latch,
            end + MS,
            loop_idle.since_ns(),
            || tx.send(()).is_ok(),
            || end + MS,
        );
        assert_eq!(gate, WakeGate::Coalesced);
        // Turn k+1 (the queued wake turns the loop at once): the drain delivers
        // the ONE wake and the handler clears.
        assert_eq!(rx.try_iter().count(), 1, "no duplicate wake was posted");
        latch.store(0, Ordering::Relaxed);
        let next = watch.pass(end + 2 * MS, [(7, &latch)]);
        assert_eq!((next.healed, next.late, next.recheck_ns), (0, 0, None));
    }

    /// Through the real scheduler, under 400 ms frames with a wake posted 5 ms
    /// into each: a late wake's recheck must never be in the past, or
    /// `metrics::record_deadline`'s stale-arm backstop fires on every turn. A late
    /// wake is queued and turns the loop by itself.
    #[test]
    fn consecutive_long_frames_never_arm_a_past_wake_heal_deadline() {
        use crate::metrics::{
            DeadlineOwner, past_arm_books, record_deadline, scheduler_state_serial,
        };
        use std::time::{Duration, Instant};
        let _serial = scheduler_state_serial();
        let epoch = Instant::now();
        let at = |ns: u64| epoch + Duration::from_nanos(ns);
        let latch = AtomicU64::new(0);
        let mut watch = OutputWakeWatch::default();
        let mut t = 1_000 * MS;
        assert_eq!(watch.pass(t, [(9, &latch)]).recheck_ns, None);
        // A healthy arm first, so no streak is inherited from another test.
        let healthy = at(t + 50 * MS);
        assert_eq!(
            record_deadline(DeadlineOwner::WakeHeal, Some(healthy), at(t)),
            Some(healthy)
        );
        let before = past_arm_books(DeadlineOwner::WakeHeal);
        for turn in 0..40 {
            // The turn's drain delivers the queued wake; the handler clears.
            latch.store(0, Ordering::Relaxed);
            // 5 ms in, output posts a wake; the frame runs 400 ms.
            let post = t + 5 * MS;
            assert_eq!(
                gated_output_wake(&latch, post, 0, || true, || post),
                WakeGate::Posted
            );
            t += 400 * MS;
            let pass = watch.pass(t, [(9, &latch)]);
            assert_eq!(pass.healed, 0, "turn {turn}: late, not lost");
            let recheck = pass
                .recheck_ns
                .expect("an outstanding wake folds a recheck");
            // `record_deadline` reads its clock after the pass.
            let wanted = at(recheck);
            assert_eq!(
                record_deadline(DeadlineOwner::WakeHeal, Some(wanted), at(t + MS)),
                Some(wanted),
                "turn {turn}: a recheck {} ms from the pass must pass through unclamped",
                (i128::from(recheck) - i128::from(t)) / i128::from(MS)
            );
        }
        assert_eq!(
            past_arm_books(DeadlineOwner::WakeHeal),
            before,
            "(stale_arm_heals, past WakeHeal arms, WakeHeal streak heals) must not move"
        );
    }

    /// A wake that IS lost after a pass saw it is still healed within its
    /// bound: the drain after its post ran and nothing cleared the latch.
    #[test]
    fn a_truly_lost_wake_still_heals_within_its_bound() {
        let latch = AtomicU64::new(0);
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let mut watch = OutputWakeWatch::default();
        let t0 = 1_000 * MS;
        watch.pass(t0, [(3, &latch)]);
        let sent = t0 + 10 * MS;
        gated_output_wake(&latch, sent, 0, || tx.send(()).is_ok(), || sent);
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
            || tx.send(()).is_ok(),
            || recheck + 1,
        );
        assert_eq!(rx.try_iter().count(), 1);
    }

    /// A wake lost together with its loop wake-up while the loop slept: no pass
    /// saw it and no recheck exists. While output streams, the reader re-posts it
    /// at the first chunk past the expiry (not a heal), exactly once; the loop
    /// turns and the handler clears.
    #[test]
    fn a_wake_stranded_with_its_loop_wake_up_is_reposted_while_output_streams() {
        let latch = AtomicU64::new(0);
        let loop_idle = LoopIdle::default();
        let mut watch = OutputWakeWatch::default();
        let t0 = 1_000 * MS;
        // The last turn saw a clear latch: no recheck, and the loop sleeps.
        assert_eq!(watch.pass(t0, [(4, &latch)]).recheck_ns, None);
        loop_idle.went_idle(t0);
        // Output 500 ms later posts a wake that is lost with its wake-up.
        let mut sends = 0;
        let sent = t0 + 500 * MS;
        let gate = gated_output_wake(
            &latch,
            sent,
            loop_idle.since_ns(),
            || {
                sends += 1;
                true
            },
            || sent,
        );
        assert_eq!(gate, WakeGate::Posted);
        // Chunks keep arriving; the loop never turns.
        let chunk = |at: u64, sends: &mut u32| {
            let now = t0 + at * MS;
            gated_output_wake(
                &latch,
                now,
                loop_idle.since_ns(),
                || {
                    *sends += 1;
                    true
                },
                || now,
            )
        };
        for at in [550, 600] {
            assert_eq!(
                chunk(at, &mut sends),
                WakeGate::Coalesced,
                "{at} ms: within the expiry"
            );
        }
        assert_eq!(
            chunk(610, &mut sends),
            WakeGate::Reposted,
            "the first chunk past the expiry re-posts"
        );
        assert_eq!(
            latch.load(Ordering::Relaxed),
            (t0 + 610 * MS) | WAKE_LATCH_SENT,
            "re-stamped: a re-post starts a new expiry"
        );
        for at in [650, 700] {
            assert_eq!(chunk(at, &mut sends), WakeGate::Coalesced, "{at} ms");
        }
        assert_eq!(sends, 2, "one post, one re-post");
        // Were the re-post lost too, the next chunk past ITS expiry re-posts
        // again: at most one event per expiry per session.
        assert_eq!(chunk(711, &mut sends), WakeGate::Reposted);
        assert_eq!(sends, 3);
        // The loop turns: nothing was proven lost, nothing is healed.
        loop_idle.turn_began();
        latch.store(0, Ordering::Relaxed);
        let pass = watch.pass(t0 + 712 * MS, [(4, &latch)]);
        assert_eq!(pass.healed, 0, "a re-post is not a heal");
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

    /// A late wake is booked ONCE however many turns it waits; a new wake on
    /// the same session is a new book.
    #[test]
    fn a_late_wake_is_booked_once() {
        let latch = AtomicU64::new((10 * MS) | WAKE_LATCH_SENT);
        let mut watch = OutputWakeWatch::default();
        // Every pass stamps AT the latch or before it, so it is never proven lost.
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

    /// A fresh live session defaults DEC 1007 (alternate scroll) on, and an app
    /// can still turn it off with ?1007l.
    #[test]
    fn alternate_scroll_defaults_on_for_a_live_session() {
        let t = new_live_terminal(10, 40, None, aterm_types::Appearance::Dark, None);
        assert!(
            t.modes().alternate_scroll,
            "DEC 1007 must default ON for a live session"
        );
        let mut t2 = new_live_terminal(10, 40, None, aterm_types::Appearance::Dark, None);
        t2.process(b"\x1b[?1007l");
        assert!(
            !t2.modes().alternate_scroll,
            "an app can still turn alternate scroll off"
        );
    }

    /// The foreground handback restores 1007 to the host's on default, so it
    /// never turns wheel scrolling in `less`, `man` or `git log` off.
    #[test]
    fn alternate_scroll_survives_a_foreground_handback() {
        let mut t = new_live_terminal(10, 40, None, aterm_types::Appearance::Dark, None);
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

    /// Every live session's alt-screen archive carries THIS process's origin: set
    /// (a bare `Terminal::new` leaves it 0), the same for two sessions of one
    /// process (the id is the process, not the session), and stable across calls.
    #[test]
    fn live_session_archive_carries_the_process_origin() {
        let a = new_live_terminal(10, 40, None, aterm_types::Appearance::Dark, None);
        let b = new_live_terminal(12, 50, None, aterm_types::Appearance::Dark, None);
        let origin = alt_archive_origin();
        assert_ne!(origin, 0, "an origin of 0 is the unset value");
        assert_eq!(origin, alt_archive_origin(), "fixed for the process");
        assert_eq!(a.alt_archive().origin(), origin);
        assert_eq!(b.alt_archive().origin(), origin);
        assert_eq!(
            aterm_core::terminal::Terminal::new(2, 2)
                .alt_archive()
                .origin(),
            0,
            "a bare engine has no host to name its origin"
        );
    }

    /// SCROLL-1 — a fresh live session attaches a tiered scrollback STORE (not the bare
    /// 10k ring), and the resolved total `scrollback_limit` actually drives ring + store
    /// — the setting that was a silent no-op while `grid.scrollback` stayed `None`.
    /// Covers the default (100k), a finite cap, and `0` ⇒ unlimited (line limit `None`).
    #[test]
    fn live_session_attaches_tiered_scrollback_driven_by_config() {
        use aterm_core::config::TerminalConfig;
        // No config: the store is present and the unified ring + store total is
        // the advertised default (`aterm_scrollback::DEFAULT_LINE_LIMIT`).
        let t = new_live_terminal(10, 40, None, aterm_types::Appearance::Dark, None);
        assert!(
            t.scrollback().is_some(),
            "SCROLL-1: a live session must attach a tiered scrollback store"
        );
        assert_eq!(
            t.grid().scrollback_line_limit(),
            Some(100_000),
            "the default ring + store total honors the 100k line limit"
        );
        // A finite config limit reaches the store (was a no-op with scrollback = None).
        let tc = TerminalConfig {
            scrollback_limit: Some(5_000),
            ..TerminalConfig::default()
        };
        let t = new_live_terminal(10, 40, Some(&tc), aterm_types::Appearance::Dark, None);
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
        let t = new_live_terminal(10, 40, Some(&tc), aterm_types::Appearance::Dark, None);
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

    /// With the store attached, history past the 10k grid ring is retained (it
    /// tiers into the store): the total scrollback exceeds 10k lines.
    #[test]
    fn live_session_retains_scrollback_past_the_10k_ring() {
        let mut t = new_live_terminal(10, 40, None, aterm_types::Appearance::Dark, None);
        // ~12k line feeds ⇒ ~12k scrolls, far past the 10k ring. Content-free is fine —
        // we count retained lines, not their bytes; the default 100k limit won't evict.
        t.process(&vec![b'\n'; 12_000]);
        let retained = t.grid().scrollback_lines();
        assert!(
            retained > 10_000,
            "SCROLL-1: history past the 10k ring must be retained (got {retained}), not dropped"
        );
    }

    /// BROKEN-2 — a session built while the OS is LIGHT reports the LIGHT scheme from
    /// the start (engine `color_scheme()` + the DSR `CSI ?996n` reply), instead of the
    /// engine's `Dark` default. This is the tab/split-spawned-after-attach case the
    /// finding reported: the pixels were light but the engine reported dark.
    #[test]
    fn live_session_adopts_the_os_color_scheme_at_construction() {
        use aterm_types::Appearance;
        // Dark (the engine default) — unchanged baseline.
        let dark = new_live_terminal(10, 40, None, Appearance::Dark, None);
        assert_eq!(dark.color_scheme(), Appearance::Dark);
        // Light desktop: the fresh engine must REPORT light, not the dark default.
        let mut light = new_live_terminal(10, 40, None, Appearance::Light, None);
        assert_eq!(
            light.color_scheme(),
            Appearance::Light,
            "BROKEN-2: a tab spawned on a light desktop must report light"
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

    #[test]
    fn file_medium_reads_a_real_file_and_places_the_image() {
        let dir = std::env::temp_dir().join(format!("aterm-kft-f-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("img.rgba");
        std::fs::File::create(&path)
            .unwrap()
            .write_all(&vec![0u8; 10 * 20 * 4]) // one 10x20 RGBA cell
            .unwrap();

        let term = term_with_resolver();
        let seq = apc("a=T,f=32,s=10,v=20,t=f", path.to_str().unwrap().as_bytes());
        crate::host_io_prefetch::feed_as_reader(&term, &seq);
        let placed = !term_lock(&term).cell_frame(24, 80).images[0].is_empty();

        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            placed,
            "t=f must read the real file via the resolver and place the image"
        );
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
    /// so before this rule any file the user could write was deleted.
    #[test]
    fn a_temp_file_is_deleted_only_where_the_protocol_allows() {
        const MARKER: &str = "tty-graphics-protocol";
        let temp_scratch = scratch_in(&std::env::temp_dir(), "t");
        let temp = &temp_scratch.0;
        // Outside every temporary directory: beside the test binary, in the
        // build's target directory (kept off /tmp).
        let exe = std::env::current_exe().unwrap();
        let target = exe.parent().and_then(Path::parent).unwrap();
        let outside_scratch = scratch_in(target, "kept");
        let outside = &outside_scratch.0;
        let canonical = std::fs::canonicalize(outside).unwrap();
        let temp_dirs = [std::path::PathBuf::from("/tmp"), std::env::temp_dir()]
            .map(|dir| std::fs::canonicalize(dir).unwrap());
        assert!(
            !temp_dirs.iter().any(|tmp| canonical.starts_with(tmp)),
            "the target directory must not be a temporary directory"
        );

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
        escaped.push(canonical.strip_prefix("/").unwrap());
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

    /// The directory a `t=t` entry is judged in is the one it is unlinked
    /// from (ruling 2026-10-10, P4): a program that swaps the marked directory
    /// for a symlink after the judgement and before the unlink, as one that
    /// can write the temporary directories can, does not carry the delete out
    /// of them.
    #[cfg(unix)]
    #[test]
    fn a_directory_swapped_after_the_judgement_moves_no_delete() {
        const MARKER: &str = "tty-graphics-protocol";
        let temp_scratch = scratch_in(&std::env::temp_dir(), "swap");
        // Outside every temporary directory: the build's target directory, as
        // in the test above.
        let exe = std::env::current_exe().unwrap();
        let target = exe.parent().and_then(Path::parent).unwrap();
        let outside_scratch = scratch_in(target, "swap-kept");
        let marked = temp_scratch.0.join(MARKER);
        std::fs::create_dir_all(&marked).unwrap();
        let sent = cell_file(&marked, "victim.rgba");
        let victim = cell_file(&outside_scratch.0, "victim.rgba");
        let held = temp_scratch.0.join("held");

        let mut swapped = false;
        let read = read_kitty_temp_file_with(&sent, || {
            std::fs::rename(&marked, &held).unwrap();
            std::os::unix::fs::symlink(&outside_scratch.0, &marked).unwrap();
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
    /// medium resolver (ruling 2026-10-10, [`host_reach`]): aterm reads no
    /// file and no shared memory its program names, so a file the profile
    /// refuses the program is neither drawn nor told apart from a missing
    /// one, and a `t=t` file is not deleted as aterm. A tab of any other mode
    /// reads the file, tells it apart from a missing one and consumes the
    /// `t=t` file, which is what makes the refusal mean something.
    #[test]
    fn a_contained_session_reads_nothing_its_program_names() {
        use aterm_containment::ContainmentMode as Cm;
        let scratch = scratch_in(&std::env::temp_dir(), "reach");
        let marked = scratch.0.join("tty-graphics-protocol");
        std::fs::create_dir_all(&marked).unwrap();
        let missing = scratch.0.join("absent.rgba");
        for mode in [Cm::User, Cm::Master, Cm::Safety, Cm::Containment] {
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

#[cfg(test)]
mod ring_budget_tests {
    use super::{
        LIVE_SCROLLBACK_RING_BYTES, LIVE_SCROLLBACK_RING_LINES_MAX, LIVE_SCROLLBACK_RING_LINES_MIN,
        live_scrollback_ring_lines,
    };

    /// The sizing formula's estimate, not a measurement of allocated memory.
    fn estimated_bytes_at(cols: u16) -> usize {
        live_scrollback_ring_lines(cols)
            * (usize::from(cols) * std::mem::size_of::<aterm_core::grid::Cell>() + 45)
    }

    /// The initial capacity scales with width under the sizing estimate, with
    /// explicit min/max line clamps. This checks that policy's arithmetic;
    /// it does not assert an allocation bound across resizes or restoration.
    #[test]
    fn the_scrollback_ring_targets_estimated_bytes_at_initial_width() {
        for cols in [80u16, 120, 200, 400, 1000] {
            let lines = live_scrollback_ring_lines(cols);
            assert!(
                (LIVE_SCROLLBACK_RING_LINES_MIN..=LIVE_SCROLLBACK_RING_LINES_MAX).contains(&lines),
                "{cols} cols: {lines} lines is outside the ring's own bounds"
            );
            // Wherever the floor is not what bound it, the estimate fits.
            if lines > LIVE_SCROLLBACK_RING_LINES_MIN {
                assert!(
                    estimated_bytes_at(cols) <= LIVE_SCROLLBACK_RING_BYTES,
                    "{cols} cols: estimated {} B over the {LIVE_SCROLLBACK_RING_BYTES} B target",
                    estimated_bytes_at(cols)
                );
            }
        }
        assert_eq!(
            live_scrollback_ring_lines(4096),
            LIVE_SCROLLBACK_RING_LINES_MIN
        );
        assert!(
            estimated_bytes_at(4096) > LIVE_SCROLLBACK_RING_BYTES,
            "the intentional floor must not be misreported as a hard byte limit"
        );
        // A wider window gets a SHALLOWER ring. That is the whole point.
        assert!(
            live_scrollback_ring_lines(80) > live_scrollback_ring_lines(400),
            "a narrow window keeps the deeper ring"
        );
        // And what it replaced — a flat 10,000 lines at 200 columns — was 16.5 MB
        // against this ring's 4.0 MiB, a factor of 3.9.
        let flat_10k = 10_000 * (200 * std::mem::size_of::<aterm_core::grid::Cell>() + 45);
        assert!(
            flat_10k > 3 * estimated_bytes_at(200),
            "the 200-column ring should be a fraction of the flat 10k it replaced: {flat_10k} B vs {} B",
            estimated_bytes_at(200)
        );
    }
}

/// R08: the render debt an adopted session brought across, on the restored
/// engine and in the resize ledger.
#[cfg(test)]
mod adoption_render_debt_tests {
    use std::sync::{Arc, Mutex};

    use aterm_core::terminal::Terminal;

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
        seed_adopted_render_debt(&term, &tl, (-1, 0));
        assert_eq!(term.lock().unwrap().render_debt(), (-1, 0));
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

    /// The first full clear (the program repainting, on the pulse or on its own)
    /// resets the debt as ever, and the epoch closes healed: nothing is left for
    /// the nudge.
    #[test]
    fn a_seeded_debt_is_reset_by_the_first_full_clear() {
        let term = restored();
        let tl = timeline();
        seed_adopted_render_debt(&term, &tl, (-1, 0));
        term.lock().unwrap().process(b"\x1b[2J\x1b[1;1Hrepainted");
        assert_eq!(term.lock().unwrap().render_debt(), (0, 0));
        let probe = {
            let t = term.lock().unwrap();
            crate::resize_ledger::EngineProbe::sample(&t)
        };
        let mut guard = tl.lock().unwrap();
        guard.set_agent_reader_for_test(true);
        guard.evaluate_resizes(probe, crate::turn_ledger::now_ms() + 60_000);
        assert!(guard.resize_nudge_view().is_none());
        assert!(guard.resizes().epochs().current().is_none(), "closed");
    }

    /// A predecessor that owed nothing (or one from before the field) hands
    /// over no debt: `spawn_session` never calls the seed, and a program on the
    /// alternate screen is exactly as before - no epoch, no nudge - whatever
    /// its class.
    #[test]
    fn a_record_without_the_key_arms_nothing_for_any_program() {
        for agent in [true, false] {
            let term = restored();
            let tl = timeline();
            let probe = {
                let t = term.lock().unwrap();
                crate::resize_ledger::EngineProbe::sample(&t)
            };
            let mut guard = tl.lock().unwrap();
            guard.set_agent_reader_for_test(agent);
            guard.evaluate_resizes(probe, crate::turn_ledger::now_ms() + 60_000);
            assert!(
                guard.resizes().epochs().current().is_none(),
                "agent={agent}"
            );
            assert!(guard.resize_nudge_view().is_none(), "agent={agent}");
        }
    }

    /// The seed refuses the main screen: the debt is about the alternate
    /// screen's rows.
    #[test]
    fn a_debt_seeded_onto_the_main_screen_is_ignored() {
        let term = Arc::new(Mutex::new(Terminal::new(12, 20)));
        let tl = timeline();
        seed_adopted_render_debt(&term, &tl, (-1, 0));
        assert_eq!(term.lock().unwrap().render_debt(), (0, 0));
        assert!(tl.lock().unwrap().resizes().epochs().current().is_none());
    }

    /// The seed sits after the hydrate (a restored checkpoint replaces the
    /// engine's modes and would zero it) and before the reader can parse a
    /// byte; the size pulse and its rule are untouched. Read from the source,
    /// the idiom of the pins above.
    #[test]
    fn the_seed_follows_the_hydrate_precedes_the_reader_and_leaves_the_pulse_alone() {
        let body = crate::spawn::spawn_session_body();
        let call = "seed_adopted_render_debt(&term, &ctx.timeline, debt);";
        assert_eq!(body.matches(call).count(), 1, "one seed");
        let seed = body.find(call).expect("called");
        let hydrate = body.find("hydrate_adopted_engine(").expect("hydrates");
        let pulse = body
            .find("adoption_needs_size_pulse(adopt_checkpoint.as_ref(), adopt_repaint)")
            .expect("the pulse rule is unchanged");
        let reader = body.find("attach_reader(").expect("attaches its reader");
        assert!(hydrate < seed, "the seed follows the hydrate");
        assert!(seed < reader, "the seed precedes the reader");
        assert!(hydrate < pulse, "the pulse still runs, after the hydrate");
        // The call is `pulse_adopted_pty(` (a hold-carrying signature since the
        // adoption pulse is held for a repaint adoption); comments name it in
        // backticks with no open parenthesis.
        assert_eq!(
            body.matches("pulse_adopted_pty(").count(),
            1,
            "the pulse is still one call, after the hydrate"
        );
        assert!(
            hydrate < body.find("pulse_adopted_pty(").unwrap(),
            "the pulse call follows the hydrate"
        );
        let src = include_str!("spawn.rs");
        let pulse_fn = src
            .find("fn pulse_adopted_pty_with(")
            .expect("the pulse is its own function");
        let pulse_fn = &src[pulse_fn..];
        let pulse_fn = &pulse_fn[..pulse_fn.find("\n}\n").expect("its body ends")];
        let down = pulse_fn
            .find("aterm_pty::resize_with_cell_px(master, transient, cols, cell_px);")
            .expect("it writes rows - 1");
        let up = pulse_fn
            .find("aterm_pty::resize_with_cell_px(master, transient, cols, cell_px);\n            aterm_pty::resize_with_cell_px(master, rows, cols, cell_px);")
            .expect("then rows, back to back");
        assert!(
            down <= up,
            "and it is still the back-to-back rows-1 -> rows pair"
        );
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
