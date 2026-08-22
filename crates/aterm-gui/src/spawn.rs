// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Session spawning + recursion provisioning: the free-fn island that builds a
//! tab's engine + PTY (`spawn_session` via `SessionFactory`), prepares shell
//! integration, and provisions a child aterm's recursion identity/edges. Plus
//! `App::register_session`. A verbatim relocation of the spawn seam.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use aterm_core::terminal::{ClipboardAccess, ClipboardOperation, Terminal};
use aterm_session::sink::SinkWriter;
use aterm_session::{EdgeTable, EdgeToken, LaunchNonce, Op, SessionId};
use winit::event_loop::EventLoopProxy;

use crate::{
    Session, SessionCtx, Wake, WindowId, control, control_auth, notify, proxy, session_store,
    term_lock,
};

/// The shell-integration loader's load-once guard variable — the env var the
/// shipped zsh/bash scripts check (`[[ -n … ]] && return`) and then `export`.
///
/// Named HERE because it is a lifeline for shell integration in a NESTED
/// aterm: an aterm spawned from a shell that itself runs inside aterm
/// inherits the parent shell's exported guard, the child loader bails, no
/// OSC 133/633 marks are ever emitted, and blocks / cwd tracking / the
/// `blocks`/`wait` verbs sit dead in that instance (the 0.19.0 gauntlet's
/// F3, first noticed through the then-per-app cursor breeds). The spawn env
/// assembly in `lib.rs` overrides the inherited value with an EMPTY string
/// for every integrated session; the test below pins this name against the
/// shipped scripts so the two can never drift apart.
pub(crate) const SHELL_INTEGRATION_LOADED_GUARD: &str = "ATERM_SHELL_INTEGRATION_INSTALLED";

/// Prepare OSC 133/633 shell integration for `$SHELL`: returns the `(key, value)`
/// environment additions + an optional argv override (bash's `--rcfile`) to
/// inject into the spawned shell so it emits the command marks the
/// `blocks`/`blocktext`/`wait` introspection verbs surface, plus the raw
/// capability nonce for `Terminal::authorize_shell_integration` so ONLY this
/// shell's marks are trusted. `None` for an unknown shell or on I/O error (the
/// shell still spawns, just without command-block tracking). Runs in the PARENT,
/// before spawn — its file I/O is not async-signal-constrained.
/// Env additions, optional argv override, and the raw capability nonce that
/// [`prepare_shell_integration`] hands back to the spawn path.
type ShellIntegrationSetup = (Vec<(String, String)>, Option<Vec<String>>, [u8; 32]);

fn prepare_shell_integration(shell_hint: Option<&str>) -> Option<ShellIntegrationSetup> {
    use aterm_core::shell_integration as si;
    // Detect from the ACTUALLY-selected shell (config `shell` / `--shell`) so a
    // non-default shell gets its OWN integration script (bash → the bash hooks),
    // not PowerShell's. `detect_current()` only sees `ATERM_SHELL`, so a shell
    // chosen via the CONFIG key would otherwise be misdetected as PowerShell.
    let shell = match shell_hint.filter(|s| !s.is_empty()) {
        Some(h) => si::ShellType::detect(h),
        None => si::ShellType::detect_current(),
    };
    let mut injection = si::prepare(shell).ok().flatten()?;
    let nonce = si::generate_nonce();
    si::augment_with_nonce(&mut injection, nonce.hex());
    Some((
        injection.env_add,
        injection.argv_override,
        nonce.into_parts().0,
    ))
}

/// Everything `spawn_session` needs to stand up a NEW tab's shell session,
/// captured ONCE at startup. The spawn/sandbox caps are the SINGLE root authority
/// minted in `main` (held by clone — cloning a `Cap` does NOT re-mint authority;
/// there is exactly one `unsafe Authority::root_authority()` in the product). The
/// baseline `env_add` is the terminal-identity env WITHOUT shell-integration vars
/// (those carry a per-tab nonce and are added fresh inside `spawn_session`).
pub(crate) struct SessionFactory {
    pub(crate) spawn_cap: aterm_cap::Cap<aterm_cap::effects::Spawn>,
    pub(crate) sandbox_cap: aterm_cap::Cap<aterm_sandbox::Sandbox>,
    /// Terminal-identity env (TERM/COLORTERM/LANG/…) shared by every tab; the
    /// shell-integration loader vars (which embed the per-tab nonce) are appended
    /// per session inside `spawn_session`, never here, so each tab's nonce is its own.
    pub(crate) env_add: Vec<(String, String)>,
    /// `-e <cmd>`: run this instead of `$SHELL` (also disables shell integration).
    pub(crate) exec_command: Option<Vec<String>>,
    /// Config `shell` / `--shell`: the interactive shell every tab spawns
    /// (discovery-resolved — `"bash"` finds Git Bash off-PATH, absolute paths
    /// verbatim). `None` → the platform default. See `aterm_pty` shell selection.
    pub(crate) shell_override: Option<String>,
    /// Config `shell_args`: extra argv passed to `shell_override` (e.g. `-l -i`).
    pub(crate) shell_args: Option<Vec<String>>,
    /// `-d <dir>`: working directory for every tab's shell.
    pub(crate) cwd: Option<String>,
    /// OS-sandbox wrap (macOS Seatbelt SBPL). `Some(profile)` ONLY in `Containment`
    /// mode on macOS — every tab's `spawn_shell` is then wrapped in `sandbox-exec
    /// -p <profile>` to deny network at the OS level (fail-closed if the wrapper is
    /// missing). `None` in every other mode → byte-identical, unwrapped spawn.
    /// Resolved ONCE from the containment decision in `main` so all tabs match.
    pub(crate) sandbox_wrap: Option<String>,
    /// Engine config (scrollback/cursor/theme/palette) applied to each tab's
    /// `Terminal`, byte-identical to the single-session path.
    pub(crate) terminal_config: Option<aterm_core::config::TerminalConfig>,
    /// Live OS color scheme (BROKEN-2). Threaded into every new session's engine at
    /// construction via `Terminal::set_color_scheme`, so a tab/split spawned AFTER the
    /// window attached agrees with the pixels and REPORTS the right scheme to apps
    /// (DEC 2031 / DSR `CSI ?996n`) instead of starting at the engine's `Dark` default.
    /// Seeded `Dark` (the engine default; the initial session is corrected at attach)
    /// and kept current by `App::sync_app_theme_to_appearance` on every OS flip.
    pub(crate) appearance: aterm_types::Appearance,
    /// Whether to inject OSC 133/633 shell integration. When true, EACH tab gets
    /// a FRESH CSPRNG nonce (a reused nonce would let one tab's output forge
    /// another tab's shell-integration marks), authorized + required on its own
    /// engine. False when `-e` runs a command or integration is opted out.
    pub(crate) integrate: bool,
    /// Latency epoch shared across tabs: each session's PTY reader stamps the
    /// leading edge of its output bursts (into the SESSION's own `last_output_ns`)
    /// on this origin, so the present path's `output->present` subtraction is
    /// valid against UI-thread reads of the same epoch. Always on — a single
    /// cheap CAS per burst (see `Session::last_output_ns`).
    pub(crate) lat_epoch: Instant,
    /// Desktop-notification delivery channel shared by every tab. Each
    /// `spawn_session` clones this `SyncSender` into the engine's notification
    /// callbacks (OSC 9/99/777); the lone delivery thread (`notify::spawn_delivery`)
    /// owns the receiver and runs the native notifier off the reader hot path. The
    /// channel is BOUNDED (`notify::NOTIFY_QUEUE_CAP`), so the callbacks `try_send`
    /// and drop on `Full` — a notification flood can never grow it unbounded.
    pub(crate) notify_tx: std::sync::mpsc::SyncSender<notify::NotifyMsg>,
    /// Security opt-in (config `allow_kitty_file_transfer`, default false): when set,
    /// each tab installs the Kitty non-direct-medium resolver so `t=f`/`t=t`/`t=s`
    /// images load from host files / shared memory. Off ⇒ those mediums skip cleanly.
    pub(crate) allow_kitty_file_transfer: bool,
    /// Opt-in (config `temporal_recording`, default false): when set, each tab runs
    /// the hydratable temporal recorder — the t0 keyframe seed, the writer thread,
    /// and the reader-hot-path `RawIn`/`Reply` taps that feed the B.9 spine (read
    /// back by the `temporal` control verb). Off ⇒ NONE of that is wired: no writer
    /// thread, no retention growth, no keyframe seed. `SessionCtx.temporal` still
    /// exists (an empty, untouched recorder costs ~0), so the field stays non-Option.
    pub(crate) temporal_recording: bool,
    /// OVERLAP HANDOFF (incoming side): when true, ADOPTED sessions are built
    /// WITHOUT their PTY reader — [`attach_reader`] runs later, after every
    /// carried window has presented and the readiness byte told the parked
    /// parent to exit (`App::maybe_signal_handoff_ready`). Until then zero
    /// bytes are consumed, so a child that dies pre-ready leaves every post-park
    /// PTY byte queued for the parent's exact resume. Set at boot iff
    /// `ATERM_HANDOFF_READY_FD` arrived;
    /// always false for fresh (non-adopted) sessions' spawns, which attach
    /// inline regardless.
    pub(crate) defer_adopted_readers: bool,
}

/// The one-time AI-discoverability hint — OPT-IN, `None` unless `$ATERM_AI_HINT` is
/// set. A transparent terminal must not inject text into the user's screen by
/// default, so the hint is OFF out of the box; discoverability is instead carried by
/// the docs (README "For AI agents", `aterm-ctl --help`, AGENTS.md) and the control
/// verbs themselves. When opted in, a single dim (SGR 2) line is injected as program
/// output into the FIRST session's engine (see [`spawn_session`]) above the initial
/// prompt, telling whatever drives the terminal that this screen is introspectable +
/// driveable via `aterm-ctl` (which auto-resolves THIS instance's socket).
fn ai_hint_banner() -> Option<String> {
    std::env::var_os("ATERM_AI_HINT")?;
    Some(
        "\x1b[2m✶ aterm: this terminal is AI-introspectable — read its live terminal state \
         and application-rendered client pixels, drive it through the application input path, \
         and measure application latency with \
         `aterm-ctl` (see `aterm-ctl --help`; `aterm-ctl metrics` for responsiveness).\
         \x1b[0m\r\n"
            .to_string(),
    )
}

/// Stand up one tab's shell session and start its PTY reader thread — the
/// security-critical factory shared by session 0 (so startup is byte-identical)
/// and every Cmd-T tab. Each session gets, INDEPENDENTLY:
///   * its OWN PTY master via `aterm_pty::spawn_shell`, using the SAME
///     by-reference spawn/sandbox caps (no second authority mint);
///   * a FRESH shell-integration nonce when `integrate` is on — generated HERE,
///     per call, then `authorize_shell_integration` + `set_require_…(true)` — so
///     one tab's output can never forge another tab's OSC 133/633 marks;
///   * its OWN OSC 52 clipboard authorization (WRITE only; QUERY denied) + a
///     dedicated pbcopy thread + callback;
///   * its OWN `standard`-profile policy engine;
///   * its OWN PTY reader thread, which tags every `Wake` (Output/Exit/Bell) with
///     this session's `id` so `user_event` routes it to the right engine.
///
/// Returns the `Session` (id + term + master) or a spawn error (caller decides
/// fatal-at-startup vs. log-and-ignore for a Cmd-T failure).
/// Whether `s` is a well-formed session id (`s-` + 20 hex chars / 80 bits), the
/// exact shape [`SessionId::generate`] produces. Used to validate an INJECTED id
/// before adopting it, so a malformed `ATERM_SESSION_ID` falls back to a fresh
/// identity rather than poisoning the fabric.
pub(crate) fn is_valid_session_id(s: &str) -> bool {
    s.len() == 22 && s.starts_with("s-") && s.as_bytes()[2..].iter().all(u8::is_ascii_hexdigit)
}

/// PURE: parse an injected ROOT identity from the recursion env values. FAIL-CLOSED
/// — adopt ONLY when BOTH a well-formed session id AND a parseable nonce are
/// present; any partial/garbled set yields `None` so the caller generates a fresh
/// identity (never a half-provisioned one). See the recursion contract (Item 4).
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

/// Read this aterm's injected root identity from the process environment — set by
/// an OUTER aterm when it spawned us. `None` (→ fresh identity) when unset or
/// malformed. Only the ROOT session (`id == 0`) adopts it, so the outer's
/// preminted edges (which name this id as `dst`) authorize against our table.
fn adopt_injected_identity() -> Option<(SessionId, LaunchNonce)> {
    use aterm_types::domain::{ENV_LAUNCH_NONCE, ENV_SESSION_ID};
    let sid = std::env::var(ENV_SESSION_ID).ok();
    let nonce = std::env::var(ENV_LAUNCH_NONCE).ok();
    parse_injected_identity(sid.as_deref(), nonce.as_deref())
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

/// Mint a fresh child identity + the three per-op capability edges (read/write/
/// signal) the PARENT (`parent_sid`) grants over the child it is about to spawn,
/// returning the env pairs to inject into the child plus the [`ChildProvision`]
/// the parent retains. The inner aterm adopts the identity and inserts the edges
/// into its own table (see [`register_injected_parent_edges`]), so the outer holds
/// read+write+signal authority over the inner session AUTOMATICALLY — no manual
/// `grant`. Minting ALL THREE ops is required or recursion would be silently
/// read-only.
/// The child's view of WHO SPAWNED IT — pure identity, no capability.
///
/// Split out of [`provision_child_recursion_env`] because the two answer different
/// questions and only one of them is conditional. `ENV_PARENT_SESSION_ID` names the
/// hosting session so a child can address it as `@self`; it grants nothing on its
/// own (the edges are what carry authority, and they stay behind the recursion
/// gate). Every session therefore gets it — including a one-shot `-e <cmd>` session,
/// which is precisely the case that hosts an agent CLI whose hooks need to find the
/// pane they are running in.
///
/// Deny-listed like the rest of the provisioning vars, so an INHERITED copy never
/// survives a hop; each direct child receives a fresh value through `env_add`.
pub(crate) fn provision_child_identity_env(parent_sid: &SessionId) -> Vec<(String, String)> {
    use aterm_types::domain::ENV_PARENT_SESSION_ID;
    vec![(
        ENV_PARENT_SESSION_ID.to_string(),
        parent_sid.as_str().to_string(),
    )]
}

pub(crate) fn provision_child_recursion_env(
    _parent_sid: &SessionId,
) -> (Vec<(String, String)>, ChildProvision) {
    use aterm_types::domain::{ENV_LAUNCH_NONCE, ENV_SESSION_ID};
    let prov = ChildProvision {
        child_sid: SessionId::generate(),
        child_nonce: LaunchNonce::generate(),
        read: EdgeToken::generate(),
        write: EdgeToken::generate(),
        signal: EdgeToken::generate(),
    };
    // ADOPTION identity (non-secret): the child's adopted id+nonce. The parent id
    // is NOT here — it is unconditional now, see `provision_child_identity_env`.
    // The edge-token SECRETS are NOT in env (audit finding F1) — the caller routes
    // them through a 0600 file (or, only if no private dir exists, the fallback env
    // channel). `prov` carries the tokens for the caller to place + retain.
    let env = vec![
        (
            ENV_SESSION_ID.to_string(),
            prov.child_sid.as_str().to_string(),
        ),
        (ENV_LAUNCH_NONCE.to_string(), prov.child_nonce.to_hex()),
    ];
    (env, prov)
}

/// Append the parent→child edge-token channel to `env`: the 0600-FILE channel
/// (only the non-secret path goes in env) when a private socket dir exists, else
/// the FALLBACK env-hex channel (tokens env-visible, with the documented same-uid
/// caveat — used only when there is no dir to hold the file). Audit finding F1.
fn append_edge_token_channel(env: &mut Vec<(String, String)>, prov: &ChildProvision) {
    use aterm_types::domain::{ENV_EDGE_READ, ENV_EDGE_SIGNAL, ENV_EDGE_TOKENS, ENV_EDGE_WRITE};
    if let Some(dir) = control_auth::socket_dir()
        && let Some(path) = proxy::write_edge_tokens(
            &dir,
            &prov.child_sid,
            &prov.read.to_hex(),
            &prov.write.to_hex(),
            &prov.signal.to_hex(),
        )
    {
        env.push((ENV_EDGE_TOKENS.to_string(), path));
        return;
    }
    // Fallback: no private dir for the secret file — inject the hexes in env.
    env.push((ENV_EDGE_READ.to_string(), prov.read.to_hex()));
    env.push((ENV_EDGE_WRITE.to_string(), prov.write.to_hex()));
    env.push((ENV_EDGE_SIGNAL.to_string(), prov.signal.to_hex()));
}

/// PURE: insert the parent-preminted edges into a child-side [`EdgeTable`] from the
/// injected env values, binding each to the child's own `self_id` (dst) and
/// `nonce`. Returns the number of edges recorded. A parent connection presenting
/// any of these tokens then `authorize`s against this table for the matching op.
/// Missing/garbled values are skipped (fail-closed per token); a missing parent id
/// records nothing.
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
    use aterm_types::domain::{
        ENV_EDGE_READ, ENV_EDGE_SIGNAL, ENV_EDGE_TOKENS, ENV_EDGE_WRITE, ENV_PARENT_SESSION_ID,
    };
    let parent = std::env::var(ENV_PARENT_SESSION_ID).ok();
    if parent.is_none() {
        return;
    }
    // Prefer the 0600-FILE channel (audit finding F1): read the secrets from the
    // path in `ATERM_EDGE_TOKENS`. The read is NON-destructive — the file PERSISTS
    // for the parent session so a child re-launched in the SAME shell (which
    // re-inherits the pinned `ATERM_EDGE_TOKENS` path) can re-read the same secrets
    // and re-install the parent edges. A consume-on-read here deleted the file after
    // the first launch, so every subsequent same-shell relaunch installed zero
    // parent edges and the outer's `@child` proxy answered `ERR auth`. The PARENT
    // owns the file's removal (`proxy::remove_edge_tokens` on child/session
    // teardown; `proxy::sweep_stale_edges` for crash leftovers). Fall back to the
    // env-hex channel only when no file path was injected (no private dir existed).
    let (read, write, signal) = match std::env::var(ENV_EDGE_TOKENS).ok() {
        Some(path) => match proxy::read_edge_tokens(&path) {
            Some((r, w, s)) => (Some(r), Some(w), Some(s)),
            None => (None, None, None),
        },
        None => (
            std::env::var(ENV_EDGE_READ).ok(),
            std::env::var(ENV_EDGE_WRITE).ok(),
            std::env::var(ENV_EDGE_SIGNAL).ok(),
        ),
    };
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
        eprintln!(
            "aterm: ATERM_PARENT_SESSION_ID set but recorded only {n}/3 parent edges — \
             some ops have no authority (malformed/duplicate/partial edge tokens)"
        );
    }
}

/// A LIVE session handed across a SEAMLESS-UPDATE re-exec (proof-carrying DSU Rung 1b):
/// its restored fabric identity + the PTY master fd/pid the NEW process re-adopts. The
/// outgoing process cleared `FD_CLOEXEC` on `master` (so it survived `execve` — proven by
/// `aterm-pty`'s `cloexec_controls_master_survival_across_exec`), so the SAME running shell
/// keeps going; [`spawn_session`] with `adopt: Some(_)` re-attaches an engine + reader to
/// it instead of forking a fresh one. The shell keeps its ORIGINAL injected env, so
/// shell-integration / recursion tokens are not re-minted here (documented degradation).
pub(crate) struct Adopted {
    /// The outgoing session's pool id, carried through so the incoming boot can place
    /// this adopted shell back into its ORIGINAL pane — the restore manifest's matching
    /// leaf records the same id (the layout↔live-fd bridge for a multi-session handoff).
    pub local_id: u64,
    /// The PTY master fd inherited across the exec (CLOEXEC was cleared before it).
    pub master: i32,
    /// The (still-running) child shell's pid == pgid.
    pub pid: i32,
    /// The session's restored stable fabric identity (from the handoff manifest).
    pub sid: SessionId,
    /// The session's restored launch nonce.
    pub nonce: LaunchNonce,
    /// SCREEN CARRY: the outgoing engine's checkpoint, when the handoff carried
    /// one. Hydrated into the fresh engine before the reader starts, so the
    /// post-update window shows the exact pre-update visible screen (prompt included)
    /// instead of booting blank over a live shell. Preexisting off-screen scrollback
    /// is intentionally outside this bounded handoff projection. `None` is retained
    /// only for non-handoff construction; authenticated modern/legacy adoption
    /// requires a checkpoint.
    pub checkpoint: Option<aterm_core::terminal::TerminalCheckpoint>,
}

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
    factory: &SessionFactory,
    proxy: &EventLoopProxy<Wake>,
    // Per-spawn working-directory override: a new tab / split / window passes the
    // focused pane's OSC-7 cwd here so it inherits the directory the user is looking
    // at (matching Terminal.app / iTerm2 / kitty / wezterm), without mutating the
    // shared `factory`. `None` (session 0, or no cwd known) ⇒ fall back to
    // `factory.cwd` (the `-d <dir>` flag, else the launch directory).
    cwd_override: Option<&str>,
    // SEAMLESS UPDATE (Rung 1b): `Some(_)` RE-ADOPTS a live shell handed across the update
    // re-exec — reuse its PTY master fd + pid and restore its identity instead of forking a
    // fresh shell. `None` = the normal fork-a-new-shell path (every existing caller).
    adopt: Option<Adopted>,
) -> std::io::Result<Session> {
    let handoff_local_id = adopt.as_ref().map(|adopted| adopted.local_id);
    // Per-tab shell integration: a FRESH nonce per session. Reusing a nonce
    // across tabs would let tab A's (untrusted) output emit tab B's authorized
    // OSC 133/633 marks; a distinct nonce per engine prevents that cross-tab
    // forgery. Computed only when integration is enabled (never under `-e`).
    let (mut env_add, argv_override, shell_nonce) = if adopt.is_some() {
        // ADOPTED session: the shell is ALREADY running, so nothing is injected into it
        // here — it keeps its ORIGINAL env (incl. the shell-integration nonce minted at
        // first spawn). No fresh nonce, no env, no argv override on this path.
        (Vec::new(), None, None)
    } else if factory.integrate {
        match prepare_shell_integration(factory.shell_override.as_deref()) {
            Some((si_env, argv_override, nonce)) => {
                let mut env = factory.env_add.clone();
                env.extend(si_env);
                (env, argv_override, Some(nonce))
            }
            None => (factory.env_add.clone(), None, None),
        }
    } else {
        (factory.env_add.clone(), None, None)
    };

    // Recursion provisioning (Item 4): this session's own fabric identity is
    // ADOPTED from the injected env for the ROOT session (so an OUTER aterm's
    // preminted edges name us correctly) and FRESH for additional tabs. Then we
    // mint a child identity + read/write/signal edges for whatever this session
    // spawns (a shell that may run an inner aterm), inject them, and retain the
    // tokens for the cross-process dial (Item 5). The env is appended AFTER
    // shell-integration vars so it always wins, and the deny-list strips any
    // INHERITED copy so provisioning never replays past one hop.
    let (self_id, self_nonce) = match &adopt {
        // ADOPTED: RESTORE the exact fabric identity from the handoff manifest so the
        // session keeps its sid/nonce across the update (edges + discovery stay valid).
        Some(a) => (a.sid.clone(), a.nonce),
        None if id == 0 => adopt_injected_identity()
            .unwrap_or_else(|| (SessionId::generate(), LaunchNonce::generate())),
        None => (SessionId::generate(), LaunchNonce::generate()),
    };
    // IDENTITY is unconditional (adoption aside, where env cannot be re-injected):
    // every child learns which session hosts it, so `@self` resolves from any
    // descendant process. This is deliberately NOT gated on `exec_command`, unlike
    // the capability provisioning below — `aterm -e claude` is exactly the case that
    // needs it, since an agent CLI's hooks address the pane through this var.
    if adopt.is_none() {
        env_add.extend(provision_child_identity_env(&self_id));
    }
    // A one-shot `-e <cmd>` session never hosts an inner aterm, so skip child
    // recursion provisioning entirely — the injected tokens + the retained
    // `ProxyEntry` would be permanently unused. Returns the child sid to retain
    // for deregistration on this session's close (else `None`).
    let child_proxy_sid = if adopt.is_some() {
        // ADOPTED: the child is already running with its FIRST-SPAWN recursion tokens; we
        // cannot re-inject env into it, so we do not re-provision (a nested inner-aterm's
        // preminted edges keep their original values — documented degradation).
        None
    } else if factory.exec_command.is_none() {
        let (mut recursion_env, child_prov) = provision_child_recursion_env(&self_id);
        // Route the edge-token SECRETS through a 0600 file (path-only in env) so a
        // sandboxed same-uid peer that inherits the env still cannot obtain them
        // (audit finding F1); falls back to env hexes only if no private dir exists.
        append_edge_token_channel(&mut recursion_env, &child_prov);
        env_add.extend(recursion_env);
        // Retain the capability over the child we are spawning so the cross-process
        // proxy (Item 5b) can present it when forwarding to the child's socket.
        proxy::register_child(child_prov.child_sid.clone(), (&child_prov).into());
        Some(child_prov.child_sid)
    } else {
        None
    };

    // Pick the child rlimit posture by containment mode: the daily-driver modes
    // (User — the default — and Master) INHERIT the launching login shell's limits,
    // so normal programs (CUDA/ML on this box, the JVM, big LTO builds, anything
    // that reserves a large virtual address space) are not constrained more than the
    // shell that started aterm. The opt-in confinement modes (Safety / Containment)
    // keep the hardened caps. Confinement in the default mode is the capability gate
    // (and, in Containment, the OS sandbox), not a blanket RLIMIT_AS that breaks
    // legitimate programs — see `aterm_sandbox::Limits::inherit`.
    // SEAMLESS UPDATE (Rung 1b): re-adopt the handed-off live master fd + pid (the
    // outgoing process cleared CLOEXEC so it survived the exec — the SAME shell keeps
    // running); otherwise FORK a fresh shell (every normal caller).
    let adopted = adopt.is_some();
    let (master, pid, adopt_checkpoint) = match adopt {
        Some(a) => {
            // FD HYGIENE: the outgoing process cleared CLOEXEC so this master
            // survived the handoff — re-arm it NOW (mirroring what forkpty does
            // for fresh masters) or it leaks into every subprocess this process
            // spawns for the rest of its life, including the NEXT update's
            // handoff child before its own deliberate clear.
            #[cfg(unix)]
            let _ = aterm_pty::set_cloexec(a.master, true);
            (a.master, a.pid, a.checkpoint)
        }
        None => {
            // Pick the child rlimit posture by containment mode: the daily-driver modes
            // (User — the default — and Master) INHERIT the launching login shell's
            // limits, so normal programs (CUDA/ML on this box, the JVM, big LTO builds,
            // anything that reserves a large virtual address space) are not constrained
            // more than the shell that started aterm. The opt-in confinement modes
            // (Safety / Containment) keep the hardened caps — see
            // `aterm_sandbox::Limits::inherit`.
            let limits = {
                use aterm_containment::ContainmentMode as Cm;
                match aterm_containment::mode_or_containment() {
                    Cm::Master | Cm::User => aterm_sandbox::Limits::inherit(),
                    // Fail-safe to confined for an unrecognized mode.
                    _ => aterm_sandbox::Limits::shell_default(),
                }
            };
            // Capture the child pid (`spawn_shell_with_pid`) so `Session::drop` can HANG
            // UP the session (SIGHUP) before closing the master — the non-blocking
            // teardown that keeps the UI thread off the tty lock (see `Session::drop`).
            let spawned = aterm_pty::spawn_shell_with_pid(
                rows,
                cols,
                &factory.spawn_cap,
                &factory.sandbox_cap,
                &env_add,
                factory.shell_override.as_deref(),
                factory.shell_args.as_deref(),
                argv_override.as_deref(),
                factory.exec_command.as_deref(),
                cwd_override.or(factory.cwd.as_deref()),
                factory.sandbox_wrap.as_deref(),
                limits,
            );
            match spawned {
                Ok(aterm_pty::SpawnedShell { master, pid }) => (master, pid, None),
                Err(e) => {
                    // The child-recursion provisioning above (the `PROXIES` entry + the
                    // 0600 edge-token file) is registered BEFORE this fallible spawn, and
                    // its ONLY cleanup is `Session::drop` — which never runs when we never
                    // build a `Session`. Mirror that teardown here so a failed spawn
                    // (forkpty EAGAIN/EMFILE at an rlimit, a missing sandbox wrapper, …)
                    // cannot permanently leak a PROXIES entry + an orphaned edge-token
                    // file across repeated failed New Tab / New Window attempts.
                    if let Some(sid) = &child_proxy_sid {
                        proxy::deregister_child(sid);
                        if let Some(dir) = crate::control_auth::socket_dir() {
                            proxy::remove_edge_tokens(&dir, sid);
                        }
                    }
                    return Err(e);
                }
            }
        }
    };

    // The ONE byte sink for this master (whole-frame atomicity across the GUI
    // keyboard path, every control writer verb, and the reader-thread query reply).
    // It OWNS the master fd: the fd is closed only when the LAST Arc<SinkWriter>
    // clone drops (after the reader thread EOFs and every window mirror / control
    // verb releases its clone), so the fd can never be closed out from under a
    // parked reader or an in-flight writer — nor recycled by a later forkpty while
    // any clone holds it. (Session::drop therefore does NOT close `master`.)
    // SAFETY: `master` is this session's forkpty master fd, freshly returned and
    // owned solely here; wrap it in an OwnedFd so the sink becomes its sole owner.
    #[cfg(unix)]
    let sink = {
        let owned_master =
            unsafe { <std::os::fd::OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(master) };
        Arc::new(SinkWriter::new_owned(owned_master))
    };
    // Windows: `master` is the opaque ConPTY registry key; `adopt` asserts the
    // same sole-ownership contract as the Unix `from_raw_fd` above (the session
    // closes when the last Arc<SinkWriter> clone drops).
    #[cfg(windows)]
    let sink = Arc::new(SinkWriter::new_owned(aterm_pty::OwnedMaster::adopt(master)));
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
    // Per-session fabric identity (day-one single local session: a fresh id+nonce).
    let ctx = Arc::new(SessionCtx {
        sink: sink.clone(),
        edges: std::sync::Mutex::new(EdgeTable::new()),
        turn_lease: std::sync::Mutex::new(None),
        self_id,
        nonce: self_nonce,
        cast: cast.clone(),
        temporal: temporal.clone(),
        byte_fanout: byte_fanout.clone(),
        turns: Arc::new(std::sync::Mutex::new(
            crate::turn_ledger::TurnLedger::default(),
        )),
        meta: std::sync::Mutex::new(crate::session_timeline::SessionMeta::default()),
        app_kitty: std::sync::Mutex::new(crate::app_kitty::AppKittySlot::default()),
        timeline: Arc::new(std::sync::Mutex::new(
            crate::session_timeline::SessionTimeline::default(),
        )),
    });
    // ROOT session only: record the edges the OUTER aterm preminted for us (from
    // our injected env), so it holds the read/write/signal authority it granted.
    if id == 0 {
        register_injected_parent_edges(&ctx);
    }

    // Build the live engine (config applied, DEC 1007 alternate-scroll defaulted ON)
    // BEFORE the reader thread starts, byte-identical to the single-session startup.
    let term = Arc::new(Mutex::new(new_live_terminal(
        rows,
        cols,
        factory.terminal_config.as_ref(),
        factory.appearance,
    )));

    // SEAMLESS SCREEN CARRY: hydrate the adopted engine with the outgoing
    // process's checkpoint BEFORE the reader starts (no engine race) and before
    // the temporal keyframe (replay must reconstruct the restored screen, not a
    // blank one). This is what makes the post-update window show the exact
    // pre-update screen — prompt included — instead of booting blank over a
    // live shell that then LOOKS dead. The engine may restore at the OLD grid
    // size; the window's first resize converges engine + PTY to the new frame.
    if let Some(cp) = &adopt_checkpoint {
        term_lock(&term).restore_checkpoint(cp);
    }

    // One-time AI-discoverability hint: OPT-IN (`$ATERM_AI_HINT`), OFF by default so a
    // transparent terminal never injects text into the user's screen. When enabled it
    // is injected as program output into the FIRST interactive session's engine,
    // BEFORE the temporal keyframe (so replay reconstructs it) and BEFORE the reader
    // starts (so it sits above the shell's first prompt). Skipped under `-e <cmd>` (a
    // one-shot command). No queries in the banner, so no `take_response` to drain.
    if id == 0
        && factory.exec_command.is_none()
        && let Some(banner) = ai_hint_banner()
    {
        term_lock(&term).process(banner.as_bytes());
    }

    // Temporal seed (B.9 / B.3.3): record the initial keyframe of the fresh,
    // configured engine before any PTY output. Replay hydrates from this keyframe
    // and folds the recorded RawIn events forward, so every instant is
    // reconstructible from t0. The fresh terminal is parser-ground (checkpoint's
    // invariant). Off any hot path — the reader thread has not started yet.
    // GATED: only when temporal recording is enabled — an off session never seeds
    // the recorder, so it pays no retention/thread cost.
    if factory.temporal_recording {
        let cp = term_lock(&term).checkpoint();
        temporal
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .record_keyframe(cp);
    }

    // Trust ONLY this tab's command marks: install its FRESH nonce and require it.
    if let Some(nonce) = shell_nonce {
        let mut t = term_lock(&term);
        t.authorize_shell_integration(nonce);
        t.set_require_shell_integration_nonce(true);
    }

    configure_clipboard(&term);
    configure_notifications(&term, &factory.notify_tx, id);
    // Kitty file/shm transfer mediums — only when the user opted in (default off).
    if factory.allow_kitty_file_transfer {
        configure_kitty_file_transfer(&term);
    }

    // POL-1: this tab's OWN `standard`-profile policy engine, installed BEFORE its
    // reader thread produces any bytes (same fail-closed posture as session 0).
    term_lock(&term).apply_policy_engine(aterm_policy::engine::PolicyEngine::new(
        aterm_policy::profiles::standard(),
    ));

    configure_bell(&term, proxy, id, window);

    // Wake coalescing: at most ONE `Wake::Output` in flight per session. The
    // reader arms it on a clear->armed edge; the main thread's handler clears it
    // BEFORE the wake's work (see the `Wake::Output` arm), so a chunk landing
    // mid-handler re-arms a fresh event and the final burst is never lost.
    // The value is the ARM TIMESTAMP (ns on the reader's latency epoch; 0 =
    // clear), so a wake the handler never consumed EXPIRES instead of latching
    // the session silent forever — see `gated_output_wake`.
    let output_wake_pending = Arc::new(AtomicU64::new(0));

    // Per-SESSION output-burst stamp (leading-edge ns on `lat_epoch`; 0 = clear).
    // Owned by this session — NOT process-global — so the present path can book
    // `output->present` only for sessions visible in the presenting window (see
    // `App::present_latency_ns`); a background window's stream no longer bleeds
    // into another window's latency numbers.
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
        id,
        term,
        master,
        pid,
        handoff_local_id,
        ctx,
        child_proxy_sid,
        output_wake_pending,
        last_output_ns,
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
    };

    // OVERLAP HANDOFF (deferred readers): when this boot is the incoming side of
    // an overlap handoff, ADOPTED sessions stay reader-less until every carried
    // window has presented and the readiness byte is written — so a child that
    // dies pre-ready has provably consumed ZERO PTY bytes and the parked parent
    // resumes with every post-park byte intact (output waits in the kernel PTY queue; a
    // flooding program blocks on `write(slave)` like Ctrl-S flow control, never
    // losing a byte). `App::maybe_signal_handoff_ready` attaches these readers.
    // Fresh (non-adopted) sessions always attach here: they have no parked twin.
    if !(adopted && factory.defer_adopted_readers) {
        attach_reader(&mut session, window, proxy, factory).map_err(std::io::Error::other)?;
    }

    // SEAMLESS: an adopted alt-screen TUI restores as STALE pixels it does not
    // know it must repaint — its last full redraw died with the old process's
    // window. Pulse the PTY size (rows-1 → rows) so the kernel delivers real
    // SIGWINCHes and the app repaints itself. Main-screen shells skip the
    // pulse: their restored screen is already exact, and a shell's line editor
    // redrawing over it would only add churn. (Deferred-reader adoptions pulse
    // here too: the repaint bytes wait in the kernel queue until attach.)
    #[cfg(unix)]
    if adopt_checkpoint
        .as_ref()
        .is_some_and(|cp| cp.modes.alternate_screen)
    {
        aterm_pty::resize(master, rows.saturating_sub(1).max(1), cols);
        aterm_pty::resize(master, rows, cols);
    }

    Ok(session)
}

/// (Re)attach the live byte pipeline to `session`: the helper threads (cast /
/// temporal / reply / compress) plus the PTY reader on a FRESH wake pipe. The
/// helpers must be (re)spawned TOGETHER with the reader because the reader holds
/// their only senders — when it exits they all wind down with it.
///
/// Three callers:
/// * every normal `spawn_session` (inline, right after the `Session` is built);
/// * the overlap-handoff CHILD starting its deferred adopted readers after the
///   readiness byte (`App::maybe_signal_handoff_ready`);
/// * the overlap-handoff PARENT resuming parked readers after a failed handoff
///   (`App::resume_parked_readers`).
#[derive(Clone)]
pub(crate) struct DeferredReaderGate {
    inner: Arc<DeferredReaderGateInner>,
}

struct DeferredReaderGateInner {
    open: AtomicBool,
    lock: Mutex<()>,
    ready: std::sync::Condvar,
}

impl DeferredReaderGate {
    #[must_use]
    pub(crate) fn closed() -> Self {
        Self {
            inner: Arc::new(DeferredReaderGateInner {
                open: AtomicBool::new(false),
                lock: Mutex::new(()),
                ready: std::sync::Condvar::new(),
            }),
        }
    }

    /// Point-of-no-failure release used only after parent Commit. All OS threads,
    /// channels, buffers, and wake descriptors were provisioned before ProofReady.
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
    pub(crate) fn wake_stopped(&self) {
        self.inner.ready.notify_all();
    }

    fn wait_until_released(&self, stop: &AtomicBool) -> bool {
        let mut guard = self.inner.lock.lock().unwrap_or_else(|p| p.into_inner());
        while !self.inner.open.load(Ordering::Acquire) {
            if stop.load(Ordering::Acquire) {
                return false;
            }
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
mod shell_integration_guard_tests {
    use super::SHELL_INTEGRATION_LOADED_GUARD;

    /// The NESTED-LAUNCH lifeline (gauntlet F3 root cause): the spawn env
    /// overrides the inherited [`SHELL_INTEGRATION_LOADED_GUARD`] with an
    /// EMPTY value, which only defuses the loader guard if the shipped
    /// scripts (a) use exactly this variable name and (b) test it with a
    /// non-empty check (`[[ -n … ]]`). Pin both so the script and the spawn
    /// scrub can never drift apart silently.
    #[test]
    fn nested_launch_guard_name_matches_the_shipped_scripts() {
        use aterm_core::shell_integration::scripts;
        let guard_test = format!("[[ -n \"${SHELL_INTEGRATION_LOADED_GUARD}\" ]]");
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
    fn identity_env_is_parent_id_only() {
        let parent = SessionId::generate();
        let env = provision_child_identity_env(&parent);
        assert_eq!(keys(&env), vec![ENV_PARENT_SESSION_ID]);
        assert_eq!(env[0].1, parent.as_str());
    }

    /// The two provisioning halves must not overlap: recursion carries ADOPTION
    /// identity (child sid + nonce) and must no longer emit the parent id, or a
    /// shell session would receive a duplicate key in `env_add`.
    #[test]
    fn recursion_env_no_longer_emits_parent_id() {
        let parent = SessionId::generate();
        let (env, _prov) = provision_child_recursion_env(&parent);
        let k = keys(&env);
        assert!(k.contains(&ENV_SESSION_ID), "adoption sid must remain");
        assert!(k.contains(&ENV_LAUNCH_NONCE), "adoption nonce must remain");
        assert!(
            !k.contains(&ENV_PARENT_SESSION_ID),
            "parent id moved to provision_child_identity_env; emitting it here too \
             would duplicate the key for shell sessions"
        );
    }

    /// The union is what a NON-exec (shell) session injects; it must still cover
    /// every var the recursion contract promised before the split.
    #[test]
    fn identity_plus_recursion_covers_the_original_contract() {
        let parent = SessionId::generate();
        let mut env = provision_child_identity_env(&parent);
        let (rec, _prov) = provision_child_recursion_env(&parent);
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
        // The negative below is only meaningful once the waiter is PROVABLY parked.
        // Without this handshake a 20ms `recv_timeout` is shorter than a single
        // scheduling delay on a loaded box, so the assertion passed whenever the
        // thread had not been scheduled at all — i.e. it passed vacuously, and a
        // regression that activated before Commit would have passed with it.
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

pub(crate) fn attach_reader(
    session: &mut Session,
    window: WindowId,
    proxy: &EventLoopProxy<Wake>,
    factory: &SessionFactory,
) -> Result<(), String> {
    let result = attach_reader_inner(session, window, proxy, factory, None);
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
pub(crate) fn prepare_deferred_reader(
    session: &mut Session,
    window: WindowId,
    proxy: &EventLoopProxy<Wake>,
    factory: &SessionFactory,
    gate: &DeferredReaderGate,
) -> Result<(), String> {
    attach_reader_inner(session, window, proxy, factory, Some(gate.clone()))
}

/// Hand a FRESH reader a clean pair of shared latches: nothing this attach's thread
/// does may be governed by state the PREVIOUS reader left behind.
///
/// `reader_stop` is the obvious half — [`park_reader`] raises it to stop the old
/// thread, and a reader that starts with it still set would exit immediately.
///
/// `output_wake_pending` is the half that was missed, and it is the more damaging
/// one. The latch is ARMED by the reader and CLEARED by the main thread's
/// `Wake::Output` handler; a park (overlap handoff, deferred reattach) can therefore
/// leave it armed with a wake that no handler will ever consume, because the session
/// it named stopped producing output the moment its reader stopped. The new reader
/// then hits [`gated_output_wake`]'s "armed and fresh" fast path and SUPPRESSES every
/// wake it would post, so its grid is mutated with no event asking the UI to look —
/// the session's screen freezes until the 100 ms self-expiry heals it (and re-freezes
/// on the next inherited arm). Clearing here, after the old thread has been joined
/// above and before the new one is spawned, means an arm can only ever be owned by
/// the reader that posted it.
fn reset_reader_latches(session: &Session) {
    session.reader_stop.store(false, Ordering::Release);
    // Relaxed matches the latch protocol everywhere else: grid content is
    // synchronized by the term mutex, the latch only governs wake delivery.
    session.output_wake_pending.store(0, Ordering::Relaxed);
}

fn attach_reader_inner(
    session: &mut Session,
    window: WindowId,
    proxy: &EventLoopProxy<Wake>,
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
    // GATED: the temporal writer thread + reader taps exist ONLY when recording is
    // enabled. Off ⇒ `None`, so the reader skips both `RawIn`/`Reply` sends and no
    // writer thread is spawned (0-cost for opt-outs).
    // Join any straggler writer from a prior attach (its reader was joined above, so it
    // has drained and exited) before this attach spawns a fresh one into the SAME
    // recorder — no cross-attach append reorder.
    if let Some(old_writer) = session.temporal_writer_join.take() {
        let _ = old_writer.join();
    }
    let (temporal_tx, temporal_writer_join) = if factory.temporal_recording {
        let (tx, join) = spawn_temporal_writer(session.ctx.temporal.clone())?;
        (Some(tx), Some(join))
    } else {
        (None, None)
    };
    // Dedicated reply-writer thread: the reader hands query replies here instead
    // of writing them inline, so it never parks on the input-pipe write.
    let reply_tx = spawn_reply_writer(session.ctx.sink.clone())?;

    // THRU-5: dedicated tier-compression worker. Only when it actually spawns do
    // we activate the offload on the engine — so a spawn failure cleanly falls
    // back to the pre-THRU-5 inline drain (the reader promoting at the 1000-line
    // threshold) rather than deferring to a worker that does not exist. Set
    // explicitly BOTH ways: a re-attach whose worker fails must deactivate the
    // offload a previous attach turned on.
    let compress_tx = spawn_compress_worker(session.term.clone());
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
        proxy: proxy.clone(),
        reply_tx,
        compress_tx,
        cast_tx,
        temporal_tx,
        byte_fanout: session.ctx.byte_fanout.clone(),
        sink: session.ctx.sink.clone(),
        lat_epoch: factory.lat_epoch,
        last_output_ns: session.last_output_ns.clone(),
        latest_output_activity_ns: session.latest_output_activity_ns.clone(),
        output_wake_pending: session.output_wake_pending.clone(),
        wake_rd,
        stop: session.reader_stop.clone(),
        start_gate,
        read_buf,
    });
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

/// PARK a session's PTY reader — the overlap handoff's quiescence step. Pokes
/// the wake pipe (the reader's loop breaks QUIETLY, exactly the `Session::drop`
/// stop path — no `Wake::Exit`, no tab close, master untouched) and waits only
/// through the caller's absolute deadline for thread completion: the ACK that
/// proves this process can no longer consume PTY bytes, so
/// the screen checkpoint taken next is gap-free and every byte after it waits in
/// the kernel queue for whoever reads next (the handoff child, or this parent's
/// own resume). Returns `false` when the reader failed to exit by `deadline`
/// (never observed in practice — the reader's only unbounded block is the poll
/// the wake pipe interrupts); the caller may proceed with today's exec-grade
/// ms-scale race as the degraded fallback. The rollback's deferred-resume wake
/// later reattaches only after the straggler is observably finished.
pub(crate) fn park_reader(session: &mut Session, deadline: Instant) -> bool {
    let Some(join) = session.reader_join.take() else {
        return true; // never attached, already parked, or deferred
    };
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

        // 1s had to cover thread creation, first scheduling, a panic unwind and a
        // panic-hook write to stderr — a process-global lock contended by every one
        // of the ~2500 tests in this binary. Crossing it makes `park_reader` return
        // false because it TIMED OUT, not because it detected the panic, so the
        // assertion below still passes and the property under test never runs.
        // A genuine failure returns true (authorizing a handoff it must refuse), so
        // the deadline is a failure bound only and costs a passing run nothing.
        assert!(!park_reader(
            &mut session,
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        ));
        assert!(session.reader_join.is_none());
    }

    /// A park leaves the wake latch armed with an event no handler will consume
    /// (the session stopped producing output when its reader stopped). Re-attach
    /// must hand the new reader a CLEAR latch, or `gated_output_wake`'s "armed and
    /// fresh" fast path swallows every wake it posts and the session's screen
    /// freezes for the latch expiry while its grid is being mutated.
    #[test]
    fn a_reattached_reader_never_inherits_a_dead_wake_arm() {
        let session = crate::stub_session(78);
        // The stale arm the old reader left: recent enough that the self-expiry
        // would not heal it for another ~100 ms.
        session
            .output_wake_pending
            .store(1_000, std::sync::atomic::Ordering::Relaxed);
        session
            .reader_stop
            .store(true, std::sync::atomic::Ordering::Release);

        super::reset_reader_latches(&session);

        assert!(
            !session
                .reader_stop
                .load(std::sync::atomic::Ordering::Acquire),
            "a fresh reader must not start already stopped"
        );
        // The observable consequence, not just the field value: the very next
        // burst's wake actually reaches the event loop.
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        super::gated_output_wake(&session.output_wake_pending, 1_001, || tx.send(()).is_ok());
        assert_eq!(
            rx.try_iter().count(),
            1,
            "the first burst after a re-attach must post its wake"
        );
    }
}

/// Build the engine for a fresh live session: the configured grid (scrollback,
/// cursor, theme, palette) plus DEC mode 1007 (alternate scroll) defaulted ON to
/// match iTerm2 (audit M5). With 1007 set, wheel events on the alt screen become
/// arrow keys, so full-screen pagers that never request mouse tracking (less, man,
/// git log) scroll under the wheel out of the box. Programs keep full control — CSI
/// ?1007l turns it off, ?1007h back on; DECRQM reports the live state. Mirrors the
/// pre-refactor `session::build_terminal` default so the default-on is unit-testable.
fn new_live_terminal(
    rows: u16,
    cols: u16,
    cfg: Option<&aterm_core::config::TerminalConfig>,
    appearance: aterm_types::Appearance,
) -> Terminal {
    // SCROLL-1: attach a tiered scrollback STORE at construction so the user-facing
    // `scrollback_lines` (and the engine memory budget) stop being silent no-ops. A
    // bare `Terminal::new` leaves `grid.scrollback = None`, which hard-caps history at
    // the 10k grid ring AND makes `apply_config`'s limit/budget setters short-circuit
    // (both read the missing store and no-op — see `config_api.rs` memory-budget /
    // scrollback-limit branches). With an in-memory tiered store attached (hot /
    // warm-LZ4 / cold-zstd, bounded by the memory budget), `apply_config` below installs
    // the real per-config line limit + budget, and lines scrolled off the ring tier into
    // the store instead of being dropped. The GUI only ever resolves the in-memory
    // backend (no disk-tier config is exposed), so no scratch path is needed here.
    // `with_defaults()` seeds a 100k-line / 100 MB STORE. The user-facing limit is now
    // one total across that store and the ring, so the no-config path below explicitly
    // applies the advertised 100k total (leaving a 90k store share). `apply_config`
    // performs the same split for configured totals, including `scrollback_lines = 0`
    // ⇒ unlimited, bounded only by the budget. The ring stays at the pre-store 10k, so
    // history ≤10k is byte-identical to the old path and the store purely extends
    // retention past it.
    let mut t = Terminal::with_scrollback(
        rows,
        cols,
        LIVE_SCROLLBACK_RING_LINES,
        aterm_core::scrollback::Scrollback::with_defaults(),
    );
    if let Some(tc) = cfg {
        t.apply_config(tc);
    } else {
        t.set_scrollback_line_limit(Some(aterm_core::scrollback::DEFAULT_LINE_LIMIT));
    }
    // BROKEN-2: tell the engine the live OS color scheme at construction, so a
    // tab/split spawned after the window attached agrees with the rendered pixels and
    // REPORTS the right scheme (DEC 2031 / DSR `CSI ?996n`) — not the engine's `Dark`
    // default. A brand-new session has no DEC-2031 subscriber yet, so this only sets
    // state (no unsolicited report is queued); `set_color_scheme` is a no-op when the
    // value already matches (the common `Dark`-desktop path). The window-attach and
    // `ThemeChanged` paths (`apply_os_color_scheme`) still own the live-flip push.
    t.set_color_scheme(appearance);
    // apply_config never touches alternate_scroll, so ordering is irrelevant; set it
    // last to make the default-on unmistakable.
    t.modes_mut().alternate_scroll = true;
    t
}

/// The fast in-memory grid ring for a live session, in lines — held at the pre-SCROLL-1
/// `Grid::new` value so recent scrollback (≤ this many lines) stays uncompressed and
/// byte-identical to the old path; older lines tier into the attached store (warm-LZ4 /
/// cold-zstd) rather than being dropped. History depth is governed by the store's line
/// limit + memory budget, not this ring.
const LIVE_SCROLLBACK_RING_LINES: usize = 10_000;

/// OSC 52 clipboard for one session: WRITE authorized (pbcopy on a dedicated thread
/// so the blocking subprocess never runs under the Terminal lock), QUERY denied —
/// handing the user's clipboard back to a program stays off. Each tab gets its own
/// authorization + callback so a background tab's yank still reaches pbcopy.
/// Drain a channel's currently-queued backlog and return the LATEST value,
/// starting from `first` (a value already `recv`'d). Non-blocking: `try_recv`
/// consumes only what is already queued, so a burst of clipboard sets collapses
/// to one last-writer-wins value and thus one `pbcopy` spawn (bounds queue
/// depth). Behaviour-identical to processing each in turn when the clipboard is
/// last-writer-wins, since only the final value survives.
fn drain_latest<T>(first: T, rx: &std::sync::mpsc::Receiver<T>) -> T {
    let mut latest = first;
    while let Ok(next) = rx.try_recv() {
        latest = next;
    }
    latest
}

fn configure_clipboard(term: &Arc<Mutex<Terminal>>) {
    let (clip_tx, clip_rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        // Coalesce an OSC-52 set flood: each set is one blocking pbcopy call
        // (in-process NSPasteboard on macOS, X11 on Linux), so an authorized
        // burst could grow the queue without bound. Clipboard is
        // last-writer-wins, so after a recv() drain the backlog and pbcopy only
        // the latest — bounding queue depth and collapsing a burst to one write.
        while let Ok(content) = clip_rx.recv() {
            control::pbcopy(&drain_latest(content, &clip_rx));
        }
    });
    let mut t = term_lock(term);
    t.authorize_clipboard_access(ClipboardAccess::Write);
    t.set_clipboard_callback(move |op| match op {
        ClipboardOperation::Set { content, .. } => {
            let _ = clip_tx.send(content);
            None
        }
        ClipboardOperation::Clear { .. } => {
            let _ = clip_tx.send(String::new());
            None
        }
        // The engine reaches this arm ONLY through a minted
        // ClipboardQueryCapability: `allow_osc52_query = true` (default FALSE)
        // or an installed policy rule decided ALLOW, and the response still
        // rides the response-capability/rate/budget gates. This arm returning
        // `None` was the last inch nobody wrote: the knob existed, was
        // documented, threaded into the engine, authorized the mint — and the
        // authorized query then answered NOTHING, so `allow_osc52_query = true`
        // was a false promise and every OSC 52 reader (remote vim/tmux
        // clipboard sync) hung exactly as if the knob did not exist.
        //
        // An AUTHORIZED query always gets an ANSWER: an empty clipboard maps
        // to an empty reply (a valid "clipboard is empty" response), never to
        // silence — silence is indistinguishable from denial, and the host
        // already decided this session is allowed to know.
        ClipboardOperation::Query { .. } => {
            #[cfg(not(target_os = "linux"))]
            {
                Some(crate::control::pbpaste().unwrap_or_default())
            }
            #[cfg(target_os = "linux")]
            {
                // X11: only the non-blocking own-selection read — a foreign
                // owner means a blocking round-trip inside the terminal lock,
                // which is worse than no answer. Partial by design; the
                // blocking-read offload is the Linux daily-driver lane's work.
                crate::control::pbpaste_owned()
            }
        }
    });
}

/// Desktop notifications for one session (OSC 9 simple / OSC 99 kitty / OSC 777).
/// Each tab authorizes its own delivery + registers its own callbacks (so a
/// BACKGROUND tab's notification still surfaces, exactly like its OSC 52 yank). The
/// callbacks fire on this tab's reader thread under the Terminal lock, so they do
/// the absolute minimum — a lock-free, NON-BLOCKING `try_send` onto the shared
/// BOUNDED delivery channel, DROPPING the message on `Full` — and never spawn the
/// notifier here (that runs on `notify`'s dedicated thread, which also applies the
/// focus-aware suppression). `try_send`-and-drop (never `send`) guarantees a
/// notification flood can neither block this reader thread nor grow the queue
/// unbounded (the channel is capped at `notify::NOTIFY_QUEUE_CAP`).
fn configure_notifications(
    term: &Arc<Mutex<Terminal>>,
    notify_tx: &std::sync::mpsc::SyncSender<notify::NotifyMsg>,
    id: u64,
) {
    let mut t = term_lock(term);
    // SECURITY: do NOT force-authorize here. `apply_config` already set
    // `modes.allow_notifications` from config (default OFF), and the engine's OSC
    // 9/99/777 handlers gate on that bit before invoking these callbacks — so the
    // documented `allow_notifications = false` opt-out actually holds. Installing
    // the callbacks unconditionally is harmless: when the bit is false the engine
    // never calls them (mirrors `configure_kitty_file_transfer`'s opt-in gate). A
    // previous unconditional `t.authorize_notifications()` here clobbered the config
    // bit to true at spawn, making the opt-out a no-op on a fresh launch.
    // OSC 9 / 777: a bare body string, no title.
    let tx = notify_tx.clone();
    t.set_notification_callback(move |body| {
        // try_send (never send): on a full BOUNDED queue this DROPS the message
        // instead of blocking the reader thread — bounding queue memory under a flood.
        let _ = tx.try_send(notify::NotifyMsg {
            session: id,
            title: None,
            body: body.to_string(),
        });
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
        let _ = tx.try_send(notify::NotifyMsg {
            session: id,
            title: n.title,
            body: n.body.unwrap_or_default(),
        });
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

/// Maximum bytes a Kitty non-direct medium may supply (matches the engine's
/// `MAX_KITTY_IMAGE_BYTES`): bounds both a huge file and a huge shm object.
const MAX_KITTY_MEDIUM_BYTES: u64 = 4 * 1024 * 1024;

/// Install the Kitty non-direct-medium resolver for one session (OPT-IN, gated by
/// `allow_kitty_file_transfer`). The engine hands us `(medium, path/name)`; we do
/// the I/O under a fail-closed policy and return the raw image bytes:
/// - `t=f` (file): read a REGULAR file, size-capped.
/// - `t=t` (temp file): read it, then DELETE it (the client made it for us).
/// - `t=s` (shared memory): `shm_open(O_RDONLY)` + `mmap` the object, copy it out,
///   then `shm_unlink` it.
///
/// The OS's own permission model bounds what is readable (our uid); the cap bounds
/// size; and this is only wired when the user opted in (default: not installed, so
/// non-direct mediums skip). The engine never touches the filesystem/shm itself.
fn configure_kitty_file_transfer(term: &Arc<Mutex<Terminal>>) {
    use aterm_core::terminal::kitty_graphics::KittyMedium;
    term_lock(term).set_kitty_file_resolver(|medium, name| match medium {
        KittyMedium::File | KittyMedium::TempFile => {
            let path = std::path::Path::new(name);
            // Open ONCE and validate the HANDLE (fstat), never the path: a
            // stat-then-reopen race lets a same-uid writer swap in a FIFO (a
            // read-to-EOF on a fed FIFO grows until OOM) or grow the file past
            // the cap between check and read. O_NONBLOCK so a swapped-in
            // writerless FIFO can't block the open; it's a no-op for regular
            // file reads.
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
            if medium == KittyMedium::TempFile {
                let _ = std::fs::remove_file(path); // consume the client's temp file
            }
            Some(bytes)
        }
        KittyMedium::SharedMemory => read_posix_shm(name),
        // Direct is handled inline by the engine; any future medium fails closed.
        _ => None,
    });
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

/// asciicast v2 recorder writer thread (design A.5.1): the reader thread hands
/// PROGRAM-OUTPUT bursts here lock-free over the returned mpsc sender — MIRRORING the
/// OSC52 clipboard thread — so JSON-escape + recorder locking never runs on the
/// reader's hot path or under `term_lock`. The burst is timestamped at FOLD time off
/// the recorder's own epoch (shared with the resize tap), and the channel is FIFO so
/// order is preserved. An idle terminal sends no bursts, so this thread parks on
/// `recv()` and the 0%-idle property holds.
/// Bound on the asciicast writer's queue. Like `REPLY_QUEUE_CAP`, this caps a per-session
/// tap so a stalled writer thread cannot let queued bursts (each an `Arc<[u8]>` up to a
/// 64 KiB PTY read) accumulate without limit under an output flood — the reader `try_send`s
/// and DROPS on a full queue rather than blocking its hot path or growing memory. Recording
/// is best-effort (the recorder already self-caps its own deque), so a dropped burst is a
/// tolerable gap, never an OOM.
const CAST_QUEUE_CAP: usize = 1024;

/// Bound for the temporal-recorder writer queue (B.9), matching [`CAST_QUEUE_CAP`]:
/// a full queue drops the burst (best-effort recording) instead of growing without
/// bound while the writer thread is behind under a flood. See [`spawn_temporal_writer`].
const TEMPORAL_QUEUE_CAP: usize = 1024;

fn spawn_cast_writer(
    cast: Arc<std::sync::Mutex<crate::cast::CastRecorder>>,
) -> Result<std::sync::mpsc::SyncSender<std::sync::Arc<[u8]>>, String> {
    let (cast_tx, cast_rx) = std::sync::mpsc::sync_channel::<std::sync::Arc<[u8]>>(CAST_QUEUE_CAP);
    std::thread::Builder::new()
        .name("aterm-cast-writer".into())
        .spawn(move || {
            while let Ok(bytes) = cast_rx.recv() {
                let mut rec = cast.lock().unwrap_or_else(|p| p.into_inner());
                let t = rec.now();
                // Hand the reader's SHARED burst straight through: the common
                // complete-burst case retains the Arc (refcount bump) instead of
                // re-copying every output byte into the event deque.
                rec.record_output_shared(t, bytes);
            }
        })
        .map_err(|error| format!("spawn cast writer: {error}"))?;
    Ok(cast_tx)
}

/// Temporal recorder writer thread (B.9): the reader hands RawIn/Reply bursts here
/// lock-free over the returned mpsc sender — the SAME bounded/drop shape as the
/// asciicast tap ([`spawn_cast_writer`]) — so the spine append + tick stamp never run
/// on the reader's hot path or under `term_lock`. FIFO preserves event order; an idle
/// terminal parks on `recv()` (0%-idle preserved). The queue is BOUNDED
/// ([`TEMPORAL_QUEUE_CAP`]): if the writer stalls under an output flood the reader
/// `try_send`s and DROPS the burst rather than letting the channel grow without bound
/// (a dropped burst is a recording gap — best-effort, exactly like the cast/reply taps
/// — never an OOM; the recorder's own byte budget already caps retained events).
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
            use crate::temporal::TemporalMsg;
            while let Ok(msg) = temporal_rx.recv() {
                let mut rec = temporal.lock().unwrap_or_else(|p| p.into_inner());
                // Hand the reader's SHARED allocation straight through (refcount
                // move, no re-copy of the burst into the blob store).
                match msg {
                    TemporalMsg::RawIn(bytes) => rec.record_raw_in_shared(bytes),
                    TemporalMsg::Reply(bytes) => rec.record_reply_shared(bytes),
                    // Appended HERE (writer thread, FIFO order) — the enqueue was
                    // ordered under `term_lock` relative to the reader's RawIn chunks.
                    TemporalMsg::Resize { rows, cols } => rec.record_resize(rows, cols),
                }
            }
        })
        .map_err(|error| format!("spawn temporal writer: {error}"))?;
    // The JOIN HANDLE lets `park_reader` DRAIN this writer before a re-attach spawns a
    // fresh one into the SAME recorder — otherwise an old-attach backlog could append
    // AFTER new-attach events across a park→re-attach boundary (cross-attach reorder).
    // The writer's `recv` loop ends once every sender clone drops (the reader's, on its
    // thread exit), then this handle joins in bounded time (the queue is capped).
    Ok((temporal_tx, join))
}

/// PTY query-reply writer thread: the reader hands DA/DSR/CPR replies
/// (`take_response()`) here lock-free over the returned mpsc sender, so it NEVER
/// writes them inline. An inline `sink.write_frame` targets the PTY INPUT pipe,
/// which can BLOCK when the child is not draining console input; that would park
/// the reader, stop it draining OUTPUT, and — with the child blocked writing into
/// a full output pipe while conhost blocks on the full input pipe — wedge the
/// whole session in an input↔output pipe deadlock (and, on Windows, hang the
/// waiter's `ClosePseudoConsole`, which drains through the reader). Off-loading
/// the write keeps the reader returning to `read()` so output always drains; FIFO
/// preserves reply order and the sink's whole-frame lock still serializes it
/// against every other writer. An idle terminal parks on `recv()`; the thread
/// ends when the reader drops the sender at EOF (releasing its `Arc<SinkWriter>`).
/// MEM-L3: the reply queue is BOUNDED. The writer drains it with a BLOCKING
/// `write_frame`, so if a local child floods DA/DSR/CPR queries yet never reads its
/// own stdin the write parks and the always-draining reader would otherwise pile
/// replies up without limit. A `sync_channel` cap + `try_send`-and-drop on the
/// producer (the reader) bounds it; a reply is dropped only under that pathological,
/// self-inflicted flood (the child isn't reading the replies it asked for anyway,
/// and it's recoverable by closing the tab). Sized generously so no real
/// capability-probe burst ever drops.
const REPLY_QUEUE_CAP: usize = 1024;

fn spawn_reply_writer(
    sink: Arc<SinkWriter>,
) -> Result<std::sync::mpsc::SyncSender<std::sync::Arc<[u8]>>, String> {
    let (reply_tx, reply_rx) =
        std::sync::mpsc::sync_channel::<std::sync::Arc<[u8]>>(REPLY_QUEUE_CAP);
    std::thread::Builder::new()
        .name("aterm-reply-writer".into())
        .spawn(move || {
            while let Ok(resp) = reply_rx.recv() {
                let _ = sink.write_frame(&resp);
            }
        })
        .map_err(|error| format!("spawn reply writer: {error}"))?;
    Ok(reply_tx)
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

/// THRU-5: per-session off-thread tier-compression worker. Parks on `recv()`
/// (0%-idle preserved — an idle terminal never signals it); on each signal it
/// promotes the engine's deferred lazy backlog into the compressed tiers in
/// bounded [`COMPRESS_BUDGET`]-line batches, releasing the term lock and yielding
/// between batches so the PTY reader and render thread are never starved. The
/// LZ4/zstd promotion spike thus runs HERE instead of inline on the reader's
/// PTY-drain critical path (a tail-latency spike generator under floods).
///
/// Returns `None` if the worker thread could not be spawned — the caller then
/// leaves the offload INACTIVE, so the reader keeps draining inline at the 1000-
/// line threshold (the pre-THRU-5 behavior). Ends when the reader drops the
/// returned sender (session drop / EOF), exactly like the reply/cast writers.
fn spawn_compress_worker(term: Arc<Mutex<Terminal>>) -> Option<std::sync::mpsc::SyncSender<()>> {
    let (tx, rx) = std::sync::mpsc::sync_channel::<()>(COMPRESS_SIGNAL_CAP);
    std::thread::Builder::new()
        .name("aterm-scrollback-compress".into())
        .spawn(move || {
            while rx.recv().is_ok() {
                // FLOOD GATE (cat-flood regression): back-to-back signals mean the
                // reader is mid-flood, and every promotion batch here contends the
                // shared term lock against the PTY drain — compression time-slicing
                // the mutex is what collapsed flood throughput after SCROLL-1. Wait
                // for the signals to go quiet before draining, with a once-per-
                // TRICKLE_INTERVAL single batch so a perpetual flood still makes
                // slow progress. Memory stays bounded meanwhile: past the
                // backpressure cap the reader drops its oldest staged lines
                // (throughput-over-depth under extreme floods; the retained
                // ring+backlog still exceeds ghostty's default cap ~3x).
                let mut last_trickle = std::time::Instant::now();
                while let Ok(()) = rx.recv_timeout(COMPRESS_QUIET_WINDOW) {
                    if last_trickle.elapsed() >= COMPRESS_TRICKLE_INTERVAL {
                        term_lock(&term).drain_lazy_bounded(COMPRESS_BUDGET);
                        last_trickle = std::time::Instant::now();
                    }
                }
                // Timeout = quiet; Disconnected = session teardown. Either way
                // fall through to the full drain below (harmless at teardown:
                // the term Arc is still alive here).
                // Drain to the low-water mark in bounded, lock-yielding batches.
                // Break the moment a batch makes NO progress (`remaining` did not
                // shrink) as well as at the low-water mark: while the store is
                // detached for an off-thread reflow, `drain_lazy_bounded` is a
                // no-op that returns the unchanged backlog, so a `remaining <=
                // LOW_WATER`-only exit would busy-spin the shared term lock for the
                // whole reflow window. No-progress ⇒ park until the next signal
                // (the reflow re-attach, or the reader's next wake) instead.
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
                    std::thread::yield_now();
                }
            }
        })
        .ok()
        .map(|_| tx)
}

/// PIPELINE SPLIT (unix): buffers circulating between the gather and parse
/// stages. 4 × 64 KiB — ghostty's empirically-tuned ring depth (<4 measurably
/// slower, >4 no gain); bounded memory, alloc-free steady state.
#[cfg(unix)]
const GATHER_RING_BUFFERS: usize = 4;

/// Batch messages from the gather stage to the parse stage (unix pipeline).
#[cfg(unix)]
enum GatherMsg {
    /// The deferred start gate released — the parse stage may post `Ready`.
    Started,
    /// One gathered batch (buffer, filled length). The parse stage returns the
    /// buffer on the free channel after processing.
    Data(Vec<u8>, usize),
    /// The master EOF'd on its own (shell exited).
    Eof,
    /// Session teardown (wake pipe / stop flag) — exit quietly.
    Wake,
}

/// The gather stage of the split PTY pipeline: a thread that does NOTHING but
/// drain the master into recycled 64 KiB batches, so the kernel's ~1 KiB tty
/// output queue is re-armed continuously while the parse stage runs
/// CONCURRENTLY — flood throughput becomes min(drain, parse) instead of their
/// serialization (the ghostty July-2026 pipeline result, re-derived for aterm).
/// Owns the master poll loop, the wake pipe, and the deferred start gate;
/// GUARANTEES a terminal `Eof`/`Wake` message (or channel close), so the parse
/// stage never needs its own timeout. Backpressure: when the parse stage holds
/// every ring buffer, the gather blocks on the free channel and the un-drained
/// master throttles the child — exactly the old single-thread behavior.
#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn spawn_pty_gather(
    master: i32,
    wake_rd: i32,
    id: u64,
    lat_epoch: Instant,
    latest_output_activity_ns: Arc<AtomicU64>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    start_gate: Option<DeferredReaderGate>,
    first_buf: Vec<u8>,
    free_rx: std::sync::mpsc::Receiver<Vec<u8>>,
    filled_tx: std::sync::mpsc::SyncSender<GatherMsg>,
    parse_in_flight: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    // This session's byte sink, purely so the `O_NONBLOCK` flip below can be
    // DECLARED to it. `O_NONBLOCK` is a property of the open file DESCRIPTION, so
    // the gather's flip applies to every writer of the fd — but the sink cannot
    // observe it, and without that knowledge its UI-thread egress must assume the
    // description might block and write one byte per `poll(2)` to stay parking-free.
    // Telling it lets a whole keystroke frame go out in a single `write(2)`: with
    // Kitty `REPORT_EVENT_TYPES` (which agent TUIs negotiate) one physical key is
    // ~5-11 bytes for press AND release, i.e. ~20-44 syscalls on the winit event
    // loop per keypress instead of ~4.
    sink: Arc<SinkWriter>,
) -> Result<(), String> {
    std::thread::Builder::new()
        .name(format!("aterm-pty-gather-{id}"))
        .spawn(move || {
            // macOS: default QoS parks this thread on E-cores whose wakeup
            // latency dwarfs the ~10µs PTY producer/consumer cadence.
            #[cfg(target_os = "macos")]
            // SAFETY: setting this thread's own QoS class; no pointers involved.
            unsafe {
                libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INITIATED, 0);
            }
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
            let _ = filled_tx.send(GatherMsg::Started);
            // NONBLOCK direct-read drain: with the master `O_NONBLOCK` the top-up
            // spins on `read(2)` itself (no `poll(0)` per ~1 KiB kernel chunk) —
            // the C-probe-fast drain cycle. Safe to flip the SHARED file
            // description because every writer (reply writer, paste, control
            // verbs, spill drainer) goes through the pollout-retry blocking
            // emulation in aterm-session's sink. If the fcntl fails the master
            // stays blocking and the poll-guarded `drain_more` is used instead.
            let nonblock = aterm_pty::set_nonblocking(master, true).is_ok();
            // Declare it to the sink so its non-parking egress may write whole frames.
            // Passed as the fcntl's ACTUAL result, never assumed: claiming non-blocking
            // on a description that still blocks would let an over-large frame park the
            // event loop inside `write(2)` — the one hazard the per-byte cadence exists
            // to dodge. `false` merely costs syscalls, so the failure direction is safe.
            sink.note_master_nonblocking(nonblock);
            // Bench instrument (ATERM_GATHER_SINK=drop), read ONCE at thread start:
            // count + discard batches to measure the pure kernel→gather drain ceiling.
            let sink_drop = crate::bench_knobs::gather_sink_drop();
            let mut sink_dropped_bytes: u64 = 0;
            let mut buf = first_buf;
            loop {
                // STOP before the next read, never between a read and its hand-off:
                // bytes already drained from the kernel MUST reach the engine (the
                // checkpoint-tear contract) — the parse stage always processes every
                // batch it was sent.
                if stop.load(std::sync::atomic::Ordering::Acquire) {
                    let _ = filled_tx.send(GatherMsg::Wake);
                    break;
                }
                match aterm_pty::read_or_wake(master, &mut buf, wake_rd) {
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
                        let _ = filled_tx.send(GatherMsg::Eof);
                        break;
                    }
                    aterm_pty::ReadOutcome::Data(n) => {
                        // Top up past the ~1 KiB kernel chunk, hand the batch over,
                        // then take a recycled buffer — blocking on the free channel
                        // IS the backpressure.
                        let filled = if nonblock {
                            aterm_pty::drain_more_nonblocking(
                                master,
                                &mut buf,
                                n,
                                wake_rd,
                                // Bench sink detaches the parse stage — bridge
                                // unconditionally (no idle cutoff to read).
                                (!sink_drop).then_some(parse_in_flight.as_ref()),
                            )
                        } else {
                            aterm_pty::drain_more(master, &mut buf, n)
                        };
                        // Timestamp the completed kernel drain before publishing the
                        // batch. The parse-side channel receive and reader join form
                        // the happens-before path to the updater's Acquire check.
                        let output_activity_now =
                            u64::try_from(lat_epoch.elapsed().as_nanos()).unwrap_or(u64::MAX);
                        latest_output_activity_ns
                            .store(output_activity_now.max(1), Ordering::Release);
                        // Bench sink: discard the batch (no hand-off, no recycle wait);
                        // Started/Eof/Wake and all read/spin timing stay identical.
                        if sink_drop {
                            sink_dropped_bytes += filled as u64;
                            continue;
                        }
                        // Counted BEFORE the send: the bridge can never see
                        // "idle" while a batch is queued unparsed, AND the
                        // reader's fetch_sub can never precede this add
                        // (usize underflow would seal the cutoff open).
                        parse_in_flight.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if filled_tx.send(GatherMsg::Data(buf, filled)).is_err() {
                            break; // parse stage gone (session torn down)
                        }
                        buf = match free_rx.recv() {
                            Ok(b) => b,
                            Err(_) => break, // parse stage gone
                        };
                    }
                }
            }
            // Bench sink: one exit-line total so the measurement can be read off.
            if sink_drop {
                eprintln!("[bench] ATERM_GATHER_SINK=drop discarded {sink_dropped_bytes} bytes");
            }
            // The gather owns the wake pipe's READ end on unix — close it on exit.
            aterm_pty::close_fd(wake_rd);
        })
        .map(|_| ())
        .map_err(|error| format!("spawn PTY gather: {error}"))
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
    proxy: EventLoopProxy<Wake>,
    /// Query-reply sink: the reader hands DA/DSR/CPR replies to the dedicated
    /// reply-writer thread over this FIFO instead of writing them inline (the
    /// inline write could block on the input pipe and deadlock the session).
    reply_tx: std::sync::mpsc::SyncSender<std::sync::Arc<[u8]>>,
    /// THRU-5 compression-worker signal (`None` when the worker could not spawn ⇒
    /// offload inactive, reader drains inline). The reader `try_send`s a token
    /// after a burst once its lazy backlog crosses `COMPRESS_SIGNAL_AT`; the
    /// worker then promotes it into the compressed tiers off this critical path.
    compress_tx: Option<std::sync::mpsc::SyncSender<()>>,
    cast_tx: std::sync::mpsc::SyncSender<std::sync::Arc<[u8]>>,
    /// `None` when temporal recording is disabled (the default): the reader then
    /// skips the `RawIn`/`Reply` spine taps entirely (no writer thread exists).
    /// BOUNDED ([`TEMPORAL_QUEUE_CAP`]) like `cast_tx`/`reply_tx` — the reader
    /// `try_send`s and drops on a full queue rather than blocking or growing unbounded.
    temporal_tx: Option<std::sync::mpsc::SyncSender<crate::temporal::TemporalMsg>>,
    byte_fanout: Arc<crate::cast::ByteFanout>,
    /// This session's byte sink — carried ONLY so the gather can declare its
    /// `O_NONBLOCK` flip (see [`spawn_pty_gather`]'s `sink` parameter). The reader
    /// itself never writes through it; replies go via `reply_tx`.
    sink: Arc<SinkWriter>,
    lat_epoch: Instant,
    last_output_ns: Arc<AtomicU64>,
    /// Most recent consumed PTY burst (ns on `lat_epoch`; 0 = none). Overwritten
    /// on every read and never cleared by a present; updater quiet admission ages
    /// this stamp independently of the first-edge presentation metric above.
    latest_output_activity_ns: Arc<AtomicU64>,
    /// In-flight `Wake::Output` coalescing latch shared with the owning `Session`
    /// (arm timestamp ns; 0 = clear): the reader posts at most one event per
    /// main-thread handler pass (the handler clears the latch before its work),
    /// bounding wake traffic during an output flood without ever losing the final
    /// burst. Self-expiring — see [`gated_output_wake`].
    output_wake_pending: Arc<AtomicU64>,
    /// Read end of this session's wake pipe (MEM-L2): `Session::drop` writes the paired
    /// write end to break this reader out of a `read` that an orphaned child keeps from
    /// EOF'ing. `-1` when no pipe could be made ⇒ the wake-pipe-less fallback poll. Owned
    /// by the reader thread, which closes it on exit.
    wake_rd: i32,
    /// Fd-free stop flag shared with the owning `Session` (MEM-L2 fallback). Only load-
    /// bearing when `wake_rd < 0` (pipe creation failed under fd exhaustion): the reader's
    /// timed fallback poll returns `Idle` on each tick and checks this flag, so a session
    /// dropped WITHOUT a usable wake pipe is still reclaimed instead of parking forever on
    /// an orphan-held master. A no-op when the wake pipe exists (the pipe wakes first).
    stop: Arc<AtomicBool>,
    /// Overlap child preparation gate. `Some` means every reader/helper resource
    /// exists, but this thread must consume zero PTY bytes until parent Commit.
    start_gate: Option<DeferredReaderGate>,
    /// Preallocated before the thread is spawned/proof is emitted.
    read_buf: Vec<u8>,
}

/// PTY reader thread for one session: read → feed this engine → wake the UI with
/// this session's id so `user_event` routes the output/EOF to the right tab.
/// Returns the thread's join handle — kept on `Session.reader_join` as the
/// overlap handoff's park ACK (see [`park_reader`]); every other caller may
/// drop it (detach) exactly as before.
fn spawn_pty_reader(w: PtyReaderWiring) -> Result<std::thread::JoinHandle<()>, String> {
    let PtyReaderWiring {
        master,
        id,
        window,
        term,
        proxy,
        reply_tx,
        compress_tx,
        cast_tx,
        temporal_tx,
        byte_fanout,
        sink,
        lat_epoch,
        last_output_ns,
        latest_output_activity_ns,
        output_wake_pending,
        wake_rd,
        stop,
        start_gate,
        read_buf,
    } = w;
    // PIPELINE SPLIT (unix): spawn the gather stage FIRST — this thread becomes
    // the parse stage, fed recycled 64 KiB batches over a bounded channel. A
    // gather-spawn failure fails session spawn exactly like a reader-spawn
    // failure (no degraded half-pipeline states).
    #[cfg(unix)]
    let (filled_rx, free_tx, parse_in_flight) = {
        let (filled_tx, filled_rx) =
            std::sync::mpsc::sync_channel::<GatherMsg>(GATHER_RING_BUFFERS + 4);
        let (free_tx, free_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(GATHER_RING_BUFFERS);
        for _ in 1..GATHER_RING_BUFFERS {
            let _ = free_tx.try_send(vec![0u8; 65_536]);
        }
        // Batches handed to the parse stage and not yet fully ingested. The
        // gather's bridge reads 0 as "parser idle — deliver now, don't park"
        // (see drain_more_nonblocking); a heuristic, so Relaxed everywhere.
        let parse_in_flight = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        spawn_pty_gather(
            master,
            wake_rd,
            id,
            lat_epoch,
            latest_output_activity_ns.clone(),
            stop.clone(),
            start_gate,
            read_buf,
            free_rx,
            filled_tx,
            parse_in_flight.clone(),
            sink,
        )?;
        (filled_rx, free_tx, parse_in_flight)
    };
    std::thread::Builder::new()
        .name(format!("aterm-pty-reader-{id}"))
        .spawn(move || {
            // macOS: default QoS parks this thread on E-cores, whose wakeup latency
            // dwarfs the ~10µs PTY producer/consumer cadence and taxes drain
            // throughput double-digit percent. The drain IS user-initiated work.
            #[cfg(target_os = "macos")]
            // SAFETY: setting this thread's own QoS class; no pointers involved.
            unsafe {
                libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INITIATED, 0);
            }
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
            // TEMPORAL geometry watermark (B.9): the (rows, cols) of the last resize this
            // reader recorded. Seeded to a `(0, 0)` SENTINEL (no real terminal is 0x0) so
            // the FIRST processed chunk always emits an `Op::Resize` to the live geometry —
            // a redundant no-op at t0 (the keyframe already carries it), but load-bearing
            // across a park -> re-attach: it re-establishes the correct width even if a
            // resize landed while this session had no reader to observe it. Only read/
            // written under `term_lock` alongside `process`, so it needs no synchronization.
            let mut last_recorded_geom: (u16, u16) = (0, 0);
            // READINESS (async-spawn path): this thread is now LIVE and about to enter
            // its read loop, so flip the registry handle `Spawning -> Alive`. Posted
            // BEFORE the first (blocking) `read` so a shell that emits NO output — or is
            // slow to — still confirms its reader promptly; a fast shell's `Spawning`
            // window is therefore vanishingly short with ZERO artificial delay. The main
            // thread serializes spawn -> `register_session` (which registers `Spawning`)
            // BEFORE it returns to the loop to drain this `Wake`, so the transition can
            // never land before the handle exists. Fire-and-forget: under headless (no
            // event loop) `send_event` simply errors and is ignored — the session stays
            // safely `Spawning` and is still fully addressable.
            // (unix posts Ready from the parse loop's `Started` arm — the gather
            // sends it right after the gate releases, before its first read, so
            // the no-output-shell confirmation timing is preserved.)
            #[cfg(windows)]
            let _ = proxy.send_event(Wake::Ready {
                session: id,
                window,
            });
            // One gathered batch through the engine + every tap, in the order that
            // keeps the glass closest to the bytes: latency stamp, THRU-2 sliced
            // process() under the term lock (+ temporal spine), compress signal,
            // query replies, coalesced wake — and only THEN the cast/byte taps,
            // whose whole-batch copy no pixel depends on.
            // Shared by the unix parse loop and the windows inline loop.
            let mut ingest = |buf: &[u8]| {
                // Stamp the leading edge of this output burst (always on; a single
                // cheap CAS) so the present path can compute output->present latency
                // for BOTH the `metrics` control verb and the $ATERM_TRACE_LATENCY
                // log. `compare_exchange(0, …)` keeps the FIRST edge of a burst that
                // spans several reads, so coalesced reads still measure the whole
                // burst.
                //
                // Stamped HERE — BEFORE the chunked process loop — not after it. The
                // slice this metric claims to report is "bytes arrived -> pixels", and
                // the term-lock wait plus the VT parse are exactly the parts that grow
                // without bound once the terminal falls behind a flood. Stamping after
                // the loop started the measurement at "the bytes are already in the
                // grid", so a terminal running seconds behind its shell still reported
                // a few-millisecond `max_present_latency_ms`: the metric hid the one
                // stall it exists to expose. Moving the stamp changes no coalescing
                // semantics (it is still a CAS from 0, still first-edge-wins) — only
                // where that edge honestly sits.
                stamp_output_arrival(
                    &last_output_ns,
                    (lat_epoch.elapsed().as_nanos() as u64).max(1),
                );
                // THRU-5: deferred-compression backlog after this burst (set under the
                // process lock below), read afterward to decide whether to wake the worker.
                let mut backlog = 0usize;
                let response = {
                    let bytes = buf;
                    // Slice the burst so the term lock is RELEASED between chunks — but pick
                    // the slice width PER HOLD (THRU-2, the ingest lock-traffic diet):
                    //   * a keystroke is waiting to echo (`metrics::input_pending`, the
                    //     lock-free key→present stamp) ⇒ FINE 8 KiB slices, so the press
                    //     path's single lock hold (app_input.rs) and the echo present never
                    //     queue behind more than ~one chunk's process time (~10-25µs) —
                    //     exactly the sluggish-typing starvation the chunking exists for;
                    //   * no key in flight (the pure `cat`/`yes` flood) ⇒ the REST of the
                    //     burst in ONE hold: 8x fewer lock round-trips against the renderer's
                    //     LOCK A/B on this same mutex. A key landing mid-hold waits at most
                    //     one whole-burst process (~75-185µs for 64 KiB — sub-frame), and the
                    //     very next hold is fine-sliced again.
                    // The VT parser is a streaming state machine, so any slicing is
                    // byte-identical to one process() of the whole burst; take_response
                    // drains the reply buffer in order, so concatenating per-chunk replies
                    // reproduces the single-call result exactly. Small bursts (a typed echo)
                    // are one chunk either way → the common interactive path is unchanged.
                    let mut acc: Option<Vec<u8>> = None;
                    let mut off = 0;
                    while off < bytes.len() {
                        let end = off
                            + ingest_chunk_width(
                                crate::metrics::input_pending(),
                                bytes.len() - off,
                            );
                        {
                            let mut t = term_lock(&term);
                            // Temporal spine (B.9): the engine geometry BEFORE this chunk is
                            // processed. An EXTERNAL resize (main-thread window resize or the
                            // cross-session `resize` verb) mutates rows/cols under term_lock
                            // while this lock is RELEASED between chunks, so it shows up in this
                            // read on the next chunk. `process()` itself never changes rows/cols
                            // in this engine — DECCOLM (mode 3) is flag-only and XTWINOPS-8 is an
                            // async host callback — so `geom_before` equals the post-process
                            // geometry, and there is no in-band resize to reconcile (a program
                            // that "resizes" via those sequences takes effect via the host, which
                            // then drives an external resize captured here). Read only when
                            // recording.
                            let geom_before = temporal_tx.as_ref().map(|_| (t.rows(), t.cols()));
                            t.process(&bytes[off..end]);
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
                            // Record THIS chunk (input + any preceding EXTERNAL resize) UNDER
                            // the term_lock, so the spine append order equals the engine op
                            // order — a mid-print resize can never be reordered against the
                            // output it split (B.2.3). The geometry diff captures a resize from
                            // ANY external path (main-thread window resize, the cross-session
                            // `resize` verb) with no per-path enqueue: those mutate rows/cols
                            // under term_lock while this lock is released between chunks, so the
                            // next chunk's `geom_before` reflects them. If the resize is dropped
                            // on a full queue, SKIP this chunk's RawIn and leave the watermark
                            // stale so the next chunk retries — output is NEVER recorded at a
                            // geometry the spine does not yet reflect (a bounded, self-healing
                            // gap, not a permanent desync). Per-chunk `Arc` (temporal is opt-in;
                            // the whole-burst `Arc` below feeds the cast + byte taps).
                            if let Some(tx) = &temporal_tx {
                                let geom_before = geom_before.unwrap_or((0, 0));
                                let geom_recorded = if geom_before == last_recorded_geom {
                                    true
                                } else if tx
                                    .try_send(crate::temporal::TemporalMsg::Resize {
                                        rows: geom_before.0,
                                        cols: geom_before.1,
                                    })
                                    .is_ok()
                                {
                                    last_recorded_geom = geom_before;
                                    true
                                } else {
                                    false // resize dropped: retry next chunk, skip this RawIn
                                };
                                if geom_recorded
                                    && tx
                                        .try_send(crate::temporal::TemporalMsg::RawIn(
                                            std::sync::Arc::from(&bytes[off..end]),
                                        ))
                                        .is_ok()
                                {
                                    // Advance the watermark to the POST-process geometry ONLY
                                    // when the RawIn was actually recorded — a dropped RawIn must
                                    // never push the spine's geometry ahead of what it reflects.
                                    // The current engine never resizes INSIDE `process()` (DECCOLM
                                    // is flag-only, XTWINOPS-8 is an async host callback), so this
                                    // equals `geom_before`; the read future-proofs a hypothetical
                                    // synchronous in-band resize.
                                    last_recorded_geom = (t.rows(), t.cols());
                                }
                            }
                        }
                        off = end;
                    }
                    acc
                };
                // THRU-5: if this burst pushed the deferred backlog past the signal
                // point, wake the compression worker to promote it into the tiers off
                // this critical path. `try_send` on the capacity-1 channel: a token
                // already queued means a drain is pending, so the drop is a harmless
                // coalesce. `None` ⇒ no worker (spawn failed / disabled); the engine's
                // offload flag is then also inactive, so the reader already drained
                // inline and there is nothing to signal.
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
                    // Hand the reply to the dedicated writer thread — NEVER write it
                    // inline. A blocking sink write on the input pipe would park this
                    // reader, stop it draining output, and deadlock the session (see
                    // `spawn_reply_writer`). The reader returns straight to `read()`.
                    // `try_send` (never `send`): on the full BOUNDED queue (MEM-L3) this
                    // DROPS the reply rather than blocking the reader — only reachable when a
                    // child floods queries without draining stdin, which is self-inflicted.
                    let _ = reply_tx.try_send(resp);
                }
                // Coalesce wakes: post `Wake::Output` only on the latch's clear->armed
                // edge (see [`gated_output_wake`] for the protocol's guarantees).
                // The arm timestamp is read HERE rather than reused from the burst's
                // arrival stamp: the latch's staleness expiry measures how long a POSTED
                // wake has gone unhandled, so arming it with a pre-parse instant would
                // charge this burst's whole parse to the main thread's handler budget
                // and trip the lost-wake heal early on a long batch.
                let now = (lat_epoch.elapsed().as_nanos() as u64).max(1);
                gated_output_wake(&output_wake_pending, now, || {
                    proxy
                        .send_event(Wake::Output {
                            session: id,
                            window,
                        })
                        .is_ok()
                });
                // asciicast tap: record the PROGRAM OUTPUT burst (`buf[..r]`) only.
                // The `take_response()` query replies above are the terminal's OWN
                // bytes and must NOT appear as `"o"` events (design A.5.1 #3). Hand
                // off lock-free; the writer thread owns the JSON-escape, the
                // timestamp, and the locking.
                // ONE heap copy of the burst, shared by the cast + byte-fanout taps via
                // Arc (both only borrow the bytes). NOTE: the TEMPORAL RawIn is NOT sent
                // here — it is enqueued PER CHUNK under the term_lock above, so a resize is
                // ordered on the spine exactly where the engine saw it; the cast tap keeps
                // the whole burst because it orders by its own timestamp timeline.
                //
                // ORDERED AFTER the wake, deliberately (touch-to-glass audit). The tap
                // is a full heap allocation plus a memcpy of the whole batch — at flood
                // bandwidth it roughly doubles the reader's ingest memory traffic — and
                // it used to sit BETWEEN the grid mutation and the wake, so every
                // presentable frame waited out a copy of bytes no pixel depends on. It
                // cannot simply be dropped: the recorder is an ALWAYS-ON rolling buffer
                // (the `cast` / `cast frames` control verbs serialize whatever the
                // session has produced so far, with no prior arming step, which is what
                // makes an unattended session observable after the fact) — unlike the
                // temporal spine, which is opt-in and therefore fully gated to `None`.
                // Recording order is unaffected: the writer thread timestamps at FOLD
                // time off the recorder's own epoch and the channel is FIFO, and no
                // caller was ever promised that a burst is recorded before the UI sees
                // it (the hand-off has always been asynchronous).
                // Bench instrument (ATERM_CAST_TAP=off): price the always-on tap.
                if !crate::bench_knobs::cast_tap_off() {
                    let burst: std::sync::Arc<[u8]> = std::sync::Arc::from(buf);
                    // `try_send`, not `send`: the cast queue is bounded (`CAST_QUEUE_CAP`). If the
                    // writer thread stalls under an output flood, DROP this burst rather than block
                    // the reader's hot path or let the queue grow without bound. Recording is
                    // best-effort; a dropped burst is a gap, never an OOM.
                    let _ = cast_tx.try_send(burst.clone());
                    // Live byte fan-out (Item 2): tee the SAME burst to any `bytes`
                    // subscribers — one refcount bump, never blocks the reader.
                    byte_fanout.tee(&burst);
                }
            };
            #[cfg(windows)]
            loop {
                // STOP is honored at the TOP of every iteration — before the next
                // read, never between a read and its processing (bytes already out
                // of the kernel queue must reach the engine or they are torn out of
                // both the queue and any checkpoint). Load-bearing for the overlap
                // park's wake-pipe-less fallback: a flooding master keeps `poll`
                // permanently readable, so the `Idle`-arm check below never runs
                // and, without this, such a reader could not be parked at all —
                // leaving a straggler to interleave reads with the handoff child.
                if stop.load(std::sync::atomic::Ordering::Acquire) {
                    break;
                }
                // MEM-L2: block on EITHER the master OR this session's wake pipe. `Wake`
                // means `Session::drop` asked us to STOP (tab/pane close) even though the
                // master hasn't EOF'd — an orphaned different-pgroup child may still hold the
                // slave. Exit QUIETLY (no `Wake::Exit`): the main thread already initiated the
                // close, and dropping this closure releases every ref (term, reply_tx, cast_tx,
                // byte_fanout, …) so the reply-writer's `SinkWriter` — which owns the master fd
                // — can finally drop and close it, instead of the whole session leaking.
                let r = match aterm_pty::read_or_wake(master, &mut buf, wake_rd) {
                    aterm_pty::ReadOutcome::Wake => break,
                    aterm_pty::ReadOutcome::Idle => {
                        // Wake-pipe-less fallback (fd exhaustion): the bounded poll ticked with
                        // no master activity. Honor the fd-free stop flag `Session::drop` raises
                        // — the substitute for the wake pipe — so an orphan-pinned reader still
                        // exits and releases the Terminal + master-owning writer (MEM-L2). Else
                        // keep waiting for output/EOF.
                        if stop.load(std::sync::atomic::Ordering::Acquire) {
                            break;
                        }
                        continue;
                    }
                    aterm_pty::ReadOutcome::Eof => {
                        // The PTY closed on its OWN (shell/`-e` exited). Route an Exit for THIS
                        // session; the main thread closes only this tab and exits the app only
                        // if it was the last (honoring `--hold`).
                        let _ = proxy.send_event(Wake::Exit {
                            session: id,
                            window,
                        });
                        break;
                    }
                    // Batched gather: top the buffer up past the first ~1 KiB kernel
                    // chunk (macOS tty outq cap) before processing, so the term-lock /
                    // taps / wake costs are paid per ~64 KiB batch instead of per KiB —
                    // the cat-flood drain-cycle fix. Interactive bursts (<1 KiB) return
                    // on the first quiet poll: no added echo latency.
                    aterm_pty::ReadOutcome::Data(n) => {
                        let filled = aterm_pty::drain_more(master, &mut buf, n);
                        let output_activity_now =
                            u64::try_from(lat_epoch.elapsed().as_nanos()).unwrap_or(u64::MAX);
                        latest_output_activity_ns
                            .store(output_activity_now.max(1), Ordering::Release);
                        filled as isize
                    }
                };
                ingest(&buf[..r as usize]);
            }
            // UNIX parse loop: batches from the gather stage. Data batches have
            // already left the kernel queue, so they are ALWAYS processed — stop
            // is enforced by the gather BEFORE its next read (the checkpoint
            // contract); the channel is guaranteed to end with Eof/Wake (or
            // close), so recv() needs no timeout or stop polling.
            #[cfg(unix)]
            // Bench instrument (ATERM_PARSE_SINK=drop), read ONCE at thread start:
            // recycle batches without engine ingest (gather+channel+recycle cost only).
            let parse_drop = crate::bench_knobs::parse_sink_drop();
            #[cfg(unix)]
            loop {
                match filled_rx.recv() {
                    Ok(GatherMsg::Started) => {
                        let _ = proxy.send_event(Wake::Ready {
                            session: id,
                            window,
                        });
                    }
                    Ok(GatherMsg::Data(batch, len)) => {
                        // Bench sink: skip ingest (and its downstream taps/wakes);
                        // the recycle path below stays exactly the same.
                        if !parse_drop {
                            ingest(&batch[..len]);
                        }
                        // Recycle the buffer; a no-op after the gather exits.
                        let _ = free_tx.try_send(batch);
                        // Ingest done: only now does this batch stop counting as
                        // parse work for the gather bridge's idle cutoff.
                        parse_in_flight.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    Ok(GatherMsg::Eof) => {
                        let _ = proxy.send_event(Wake::Exit {
                            session: id,
                            window,
                        });
                        break;
                    }
                    Ok(GatherMsg::Wake) | Err(_) => break,
                }
            }
            // Windows: this thread owns the wake pipe's READ end — close it on
            // exit (the write end is owned + closed by `Session::drop`). No-op
            // for the `-1` sentinel. On unix the GATHER owns + closes it.
            #[cfg(windows)]
            aterm_pty::close_fd(wake_rd);
        })
        .map_err(|error| format!("spawn PTY reader: {error}"))
}

/// THRU-2: the term-lock hold width for one ingest slice of a PTY burst. FINE
/// (8 KiB) only while a keystroke is waiting to echo (`pending` — the
/// lock-free [`crate::metrics::input_pending`] signal), so the press path and
/// the echo present never queue behind more than ~one chunk's process time; a
/// WIDER but still CAPPED slice otherwise (see `IDLE_CHUNK`), so a pure output
/// flood pays a quarter of the lock round-trips while a key that arrives after
/// the hold began still waits out at most that cap. Pure; the reader
/// re-evaluates it per hold, so a key arriving mid-flood shrinks the very next
/// slice.
fn ingest_chunk_width(pending: bool, remaining: usize) -> usize {
    const PROCESS_CHUNK: usize = 8 * 1024;
    // The IDLE width is CAPPED, not unbounded (touch-to-glass audit): `pending` is
    // sampled per hold, so an unbounded remainder meant a key arriving one
    // microsecond AFTER the reader entered a whole-64-KiB hold waited out the
    // ENTIRE batch — the loop's re-evaluation never runs because the idle branch
    // produces exactly one iteration. Capping the idle hold bounds that worst-case
    // wait to ~one quarter of a full burst while still paying 4x fewer lock
    // round-trips than the fine slice.
    const IDLE_CHUNK: usize = 16 * 1024;
    if pending {
        PROCESS_CHUNK.min(remaining)
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
        assert_eq!(ingest_chunk_width(true, 64 * 1024), 8 * 1024);
        assert_eq!(ingest_chunk_width(true, 3 * 1024), 3 * 1024); // tail < chunk
        assert_eq!(ingest_chunk_width(false, 64 * 1024), 16 * 1024);
        assert_eq!(ingest_chunk_width(false, 12 * 1024), 12 * 1024); // tail < cap
        assert_eq!(ingest_chunk_width(false, 1), 1);
        // The idle hold is always at least as wide as the pending one — the
        // interactivity signal must never make the reader slower.
        assert!(ingest_chunk_width(false, 64 * 1024) >= ingest_chunk_width(true, 64 * 1024));
        // Degenerate zero-remainder never yields a zero-progress loop upstream:
        // the reader's loop condition (`off < bytes.len()`) is what prevents a
        // zero-width call; document the pure fn's behavior anyway.
        assert_eq!(ingest_chunk_width(true, 0), 0);
        assert_eq!(ingest_chunk_width(false, 0), 0);
    }
}

/// Arm the output->present measurement for a burst that is ABOUT TO BE PARSED.
///
/// `compare_exchange` from 0 (never a plain store): the first unpresented edge wins,
/// so a burst that spans several PTY reads — or several gathered batches — is measured
/// from where the terminal first fell behind, not from its final chunk. The present
/// path consumes the stamp with a `swap(0)`, which re-opens the CAS for the next edge.
/// `now_ns` is forced non-zero by the caller because 0 is the "no sample" sentinel.
///
/// Called BEFORE the engine sees the bytes. The parse and the term-lock wait are the
/// terms that blow up when the terminal falls behind a flood, so stamping after them
/// (the original placement) measured a slice that began at "already in the grid" and
/// reported single-digit milliseconds while the screen ran seconds behind the shell.
fn stamp_output_arrival(last_output_ns: &AtomicU64, now_ns: u64) {
    let _ = last_output_ns.compare_exchange(0, now_ns, Ordering::Relaxed, Ordering::Relaxed);
}

#[cfg(test)]
mod output_edge_stamp_tests {
    use super::stamp_output_arrival;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// FIRST-EDGE-WINS is the property the pre-parse move had to preserve: several
    /// bursts arriving before one present still measure from the earliest of them,
    /// and the present's `swap(0)` re-opens the stamp for the next edge.
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

/// A `Wake::Output` armed longer than this without a handler clear is presumed
/// LOST (the event never completed a handler pass — e.g. dropped around startup
/// while the winit handler was not yet ready) and the next output burst re-sends.
/// Well beyond any legitimate main-thread handler pass (~12x `MIN_FRAME_INTERVAL`),
/// so a merely-busy frame never double-wakes; a genuinely lost wake degrades to a
/// ≤100 ms hiccup instead of silencing the session's presents for the process
/// lifetime (the 2026-07-05 5 fps incident's candidate mechanism).
pub(crate) const WAKE_LATCH_EXPIRY_NS: u64 = 100_000_000;

/// Edge-triggered `Wake::Output` gate (wake coalescing): invoke `send` only when
/// the latch transitions clear->armed, so a sustained output flood queues at most
/// ONE user event per main-thread handler pass instead of one per PTY read. The
/// `Wake::Output` handler clears the latch (stores 0) BEFORE its work, so a chunk
/// processed after the clear re-arms a fresh event (at most one spurious extra
/// wake) and the final burst of a flood is never lost; the CAS is an RMW, so a
/// concurrent clear is never read stale. `Relaxed` suffices — grid content is
/// synchronized by the term mutex; the latch only governs wake delivery. A failed
/// send (headless: no event loop, see the `Wake::Ready` note in
/// [`spawn_pty_reader`]) MUST reset the latch, or the stale arm would suppress
/// sends until expiry.
///
/// SELF-EXPIRING: `now_ns` (the reader's latency-epoch clock, non-zero) is stored
/// as the arm timestamp; an arm older than [`WAKE_LATCH_EXPIRY_NS`] no longer
/// suppresses the send — the wake is presumed lost, the latch re-arms, and the
/// heal is counted (`metrics` verb `wake_heals`) + logged so a lost-wake bug is
/// VISIBLE instead of a permanent silent present-starvation.
fn gated_output_wake(flag: &AtomicU64, now_ns: u64, send: impl FnOnce() -> bool) {
    let prev = flag.load(Ordering::Relaxed);
    let stale = prev != 0 && now_ns.saturating_sub(prev) > WAKE_LATCH_EXPIRY_NS;
    if prev != 0 && !stale {
        return; // armed and fresh: the coalescing fast path
    }
    // CAS from the observed state so concurrent readers arm exactly once; a lost
    // race means someone else just armed — nothing to do.
    if flag
        .compare_exchange(prev, now_ns, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
    {
        if stale {
            crate::metrics::note_wake_heal();
            aterm_log::warn!(
                "healed a lost output wake (latch stale {} ms)",
                now_ns.saturating_sub(prev) / 1_000_000
            );
        }
        if !send() {
            flag.store(0, Ordering::Relaxed);
        }
    }
}

impl crate::App {
    /// Register a session's live handle into the process-wide registry (P1.1). The
    /// `term`/`sink`/`ctx` `Arc`s are SHARED with the owning `Session`, so a
    /// cross-session read is zero-copy. Called at the spawn seams (`open_tab` and
    /// the startup `session0`); deregistration is at the close seam (`close_tab_at`).
    ///
    /// The handle is registered `Spawning`: its engine + PTY master + sink already
    /// exist (so input and cross-session reads are immediately safe — bytes written
    /// to the PTY before the shell drains them just buffer in the kernel), but its
    /// own reader thread has not yet confirmed its first live iteration. That reader
    /// flips it to `Alive` by posting `Wake::Ready` (see [`spawn_pty_reader`]),
    /// handled on the main thread via [`session_store::SessionStore::mark_alive`].
    /// A fast shell makes the `Spawning` window vanishingly short — there is NO
    /// artificial delay; a slow shell stays `Spawning` (and fully addressable) until
    /// its reader is confirmed, so a sluggish shell init never blocks the GUI.
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
        store
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .register(handle);
        // Sibling discovery: publish this session's graph entry (sid → our
        // instance socket) so an `@<sid>` arriving at ANOTHER same-uid instance
        // can be forwarded here. No-op until/unless the control socket is bound.
        crate::proxy::publish_session(&session.ctx.self_id, &session.ctx.nonce);
    }
}

/// CLIENT-2: when aterm runs from a macOS .app bundle, its co-located CLI tools
/// (`aterm-ctl`, `atpkg`) sit NEXT TO the executable in `Contents/MacOS` — return the
/// `("PATH", value)` pair that prepends that directory to the child sessions' PATH, so
/// the workflows the bundled Help teaches (`aterm-ctl send/text/image/…`) run in every
/// aterm shell with ZERO install step. `None` outside a bundle (a dev-tree
/// `target/release/aterm-gui` resolves tools from the workspace as before) and when the
/// directory is already on the inherited PATH (an idempotent aterm-inside-aterm nesting
/// doesn't stack duplicates).
pub(crate) fn bundle_path_env(
    exe: Option<&std::path::Path>,
    inherited: Option<&str>,
) -> Option<(String, String)> {
    let dir = exe?.parent()?;
    // Component-wise suffix match: only a real `<Name>.app/Contents/MacOS` layout
    // qualifies (the shape codesign seals), so this is inert for dev/CI binaries.
    if !dir.ends_with("Contents/MacOS") {
        return None;
    }
    let dir_s = dir.to_str()?;
    match inherited {
        Some(p) if p.split(':').any(|c| c == dir_s) => None, // already reachable
        Some(p) if !p.is_empty() => Some(("PATH".to_string(), format!("{dir_s}:{p}"))),
        _ => Some(("PATH".to_string(), dir_s.to_string())),
    }
}

#[cfg(test)]
mod bundle_path_env_tests {
    use super::bundle_path_env;
    use std::path::Path;

    /// Inside a .app bundle, the executable's own directory is prepended to the
    /// inherited PATH — the co-located `aterm-ctl`/`atpkg` become runnable in every
    /// spawned shell without an installer.
    #[test]
    fn bundle_dir_is_prepended_to_the_inherited_path() {
        let exe = Path::new("/Applications/aterm.app/Contents/MacOS/aterm");
        let got = bundle_path_env(Some(exe), Some("/usr/bin:/bin"));
        assert_eq!(
            got,
            Some((
                "PATH".to_string(),
                "/Applications/aterm.app/Contents/MacOS:/usr/bin:/bin".to_string()
            ))
        );
        // No inherited PATH at all (odd launchd edge): the bundle dir alone.
        let got = bundle_path_env(Some(exe), None).expect("still injects");
        assert_eq!(got.1, "/Applications/aterm.app/Contents/MacOS");
    }

    /// Inert outside a bundle (dev builds), idempotent when the dir is already on
    /// PATH (aterm-inside-aterm nesting), and safe with no exe at all.
    #[test]
    fn dev_builds_and_nested_sessions_inject_nothing() {
        let dev = Path::new("/Users//u/aterm/target/release/aterm-gui");
        assert_eq!(bundle_path_env(Some(dev), Some("/usr/bin")), None);
        let exe = Path::new("/Applications/aterm.app/Contents/MacOS/aterm");
        let already = "/Applications/aterm.app/Contents/MacOS:/usr/bin";
        assert_eq!(bundle_path_env(Some(exe), Some(already)), None);
        assert_eq!(bundle_path_env(None, Some("/usr/bin")), None);
    }
}

#[cfg(test)]
mod clipboard_coalesce_tests {
    use super::drain_latest;

    /// A backlog of clipboard sets collapses to the LAST value (last-writer-wins),
    /// so one pbcopy spawns per burst instead of one per set. Regression: the
    /// consumer used to `pbcopy` every queued message unconditionally.
    #[test]
    fn drain_latest_returns_the_final_queued_value() {
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        // Simulate an OSC-52 set flood already sitting in the queue.
        tx.send("a".to_string()).unwrap();
        tx.send("b".to_string()).unwrap();
        tx.send("c".to_string()).unwrap();
        // `first` is the value the consumer already recv'd; drain the rest.
        let first = "first".to_string();
        assert_eq!(
            drain_latest(first, &rx),
            "c",
            "drains backlog to the latest"
        );
    }

    /// With an empty backlog, `drain_latest` returns `first` unchanged — the
    /// common single-set path is byte-identical to the old behaviour.
    #[test]
    fn drain_latest_with_empty_backlog_returns_first() {
        let (_tx, rx) = std::sync::mpsc::channel::<String>();
        assert_eq!(drain_latest("only".to_string(), &rx), "only");
    }
}

#[cfg(test)]
mod output_wake_coalesce_tests {
    use super::WAKE_LATCH_EXPIRY_NS;
    use super::gated_output_wake;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A flood of chunks with no intervening handler pass queues exactly ONE
    /// event; the handler's clear-before-work re-arms the next chunk, so the
    /// final burst is never lost. (An mpsc sender stands in for the proxy.)
    #[test]
    fn flood_queues_one_event_per_handler_pass() {
        let flag = AtomicU64::new(0);
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        for _ in 0..1000 {
            gated_output_wake(&flag, 1, || tx.send(()).is_ok());
        }
        assert_eq!(rx.try_iter().count(), 1, "flood coalesces to one event");
        // Handler pass: clear FIRST (mirrors the `Wake::Output` arm), then the
        // next chunk posts a fresh event.
        flag.store(0, Ordering::Relaxed);
        gated_output_wake(&flag, 2, || tx.send(()).is_ok());
        assert_eq!(rx.try_iter().count(), 1, "final burst re-arms a fresh wake");
    }

    /// A failed send (headless: no event loop) must RESET the latch — a stale
    /// arm would suppress wakes until expiry.
    #[test]
    fn failed_send_resets_the_flag() {
        let flag = AtomicU64::new(0);
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        drop(rx); // no event loop: every send errors
        gated_output_wake(&flag, 1, || tx.send(()).is_ok());
        assert_eq!(
            flag.load(Ordering::Relaxed),
            0,
            "failed send must not leave the latch armed"
        );
        // A later live receiver is still woken.
        let (tx2, rx2) = std::sync::mpsc::channel::<()>();
        gated_output_wake(&flag, 2, || tx2.send(()).is_ok());
        assert_eq!(rx2.try_iter().count(), 1);
    }

    /// While an event is in flight (latch armed, FRESH), `send` is not even
    /// invoked — the gate's cost during a flood is one Relaxed load per chunk.
    #[test]
    fn armed_flag_skips_the_send_entirely() {
        let flag = AtomicU64::new(1);
        gated_output_wake(&flag, 2, || panic!("send must not run while armed"));
        assert_eq!(flag.load(Ordering::Relaxed), 1, "stays armed");
    }

    /// SELF-EXPIRY (the 2026-07-05 lost-wake heal): an arm older than
    /// [`WAKE_LATCH_EXPIRY_NS`] no longer suppresses — the latch re-arms at the
    /// new timestamp and the send fires, so one lost `Wake::Output` costs a
    /// bounded hiccup instead of silencing the session's presents forever.
    #[test]
    fn stale_arm_expires_and_resends() {
        let flag = AtomicU64::new(1); // armed at t=1ns, handler never cleared it
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let later = 2 + WAKE_LATCH_EXPIRY_NS; // beyond expiry from t=1
        gated_output_wake(&flag, later, || tx.send(()).is_ok());
        assert_eq!(rx.try_iter().count(), 1, "stale arm re-sends the wake");
        assert_eq!(
            flag.load(Ordering::Relaxed),
            later,
            "latch re-armed at the fresh timestamp"
        );
        // Still within expiry of the NEW arm: coalesces again.
        gated_output_wake(&flag, later + 1, || panic!("fresh re-arm must coalesce"));
    }

    /// TIER-1 CONFORMANCE BIND (AGENTS.md: a model that maps to a real subsystem
    /// binds to the real code, with a negative control): walk every state of
    /// `aterm_spec::derive::damage_to_present_model()` reachable under the
    /// committed `Buggy = 0`, and at each one assert the REAL
    /// [`gated_output_wake`] makes exactly the decision the model's enabled
    /// `Damage*` action prescribes — `DamageFresh`/`DamageHeals` ⇔ the send
    /// fires and the latch re-arms at `now`; `DamageCoalesced`-only ⇔ no send,
    /// latch untouched. Time scaling: `K` ns per model tick, bracketed so the
    /// model's `Expiry` ticks and the code's [`WAKE_LATCH_EXPIRY_NS`] agree
    /// exactly on both sides of the staleness boundary. (The model-only
    /// `inflight` distinction is sound here because its guarded Tick makes
    /// stale-while-inflight unreachable — every reachable stale state is a
    /// lost-wake state, exactly the case the code's expiry must resend.)
    ///
    /// NEGATIVE CONTROL: the pre-fix ONE-SHOT semantics (send iff clear — the
    /// shipped 2026-07-05 defect) must DISAGREE with the model on at least one
    /// reachable state, so a pass is never vacuous.
    #[test]
    fn latch_conforms_to_damage_to_present_model() {
        let m = aterm_spec::derive::damage_to_present_model();
        let expiry: u64 = 3; // the model's Expiry constant
        // K per the boundary bracketing: K*Expiry <= EXPIRY_NS < K*(Expiry+1),
        // so (n-l)*K > EXPIRY_NS  <=>  n-l > Expiry, exactly.
        let k: u64 = 30_000_000;
        assert!(k * expiry <= WAKE_LATCH_EXPIRY_NS);
        assert!(k * (expiry + 1) > WAKE_LATCH_EXPIRY_NS);

        let actions = [
            "DamageFresh",
            "DamageCoalesced",
            "DamageHeals",
            "Lose",
            "Present",
            "Tick",
        ];
        let mut frontier = vec![m.init_state()];
        let mut seen = std::collections::BTreeSet::new();
        seen.insert(frontier[0].clone());
        let mut checked = 0_u32;
        let mut negative_control_diverged = false;
        while let Some(st) = frontier.pop() {
            let (latch_t, now_t) = (st["latch"] as u64, st["now"] as u64);
            // The model's decision for a damage event in this state.
            let fresh = !m.successors("DamageFresh", &st).is_empty();
            let heals = !m.successors("DamageHeals", &st).is_empty();
            let model_sends = fresh || heals;
            // The REAL latch, primed with this state's arm/clock (scaled).
            let flag = AtomicU64::new(latch_t * k);
            let now_ns = (now_t * k).max(1);
            let mut sent = false;
            gated_output_wake(&flag, now_ns, || {
                sent = true;
                true
            });
            assert_eq!(
                sent, model_sends,
                "gated_output_wake vs model at state {st:?}"
            );
            if model_sends {
                assert_eq!(
                    flag.load(Ordering::Relaxed),
                    now_ns,
                    "a fired send must re-arm the latch at now (state {st:?})"
                );
            } else {
                assert_eq!(
                    flag.load(Ordering::Relaxed),
                    latch_t * k,
                    "coalesced damage must leave the arm untouched (state {st:?})"
                );
            }
            // Negative control: the shipped one-shot latch (send iff clear).
            if (latch_t == 0) != model_sends {
                negative_control_diverged = true;
            }
            checked += 1;
            for a in actions {
                for nxt in m.successors(a, &st) {
                    if seen.insert(nxt.clone()) {
                        frontier.push(nxt);
                    }
                }
            }
        }
        assert!(checked > 10, "reachable-state walk degenerate ({checked})");
        assert!(
            negative_control_diverged,
            "the one-shot (pre-fix) latch must disagree with the model somewhere \
             reachable, or this bind cannot catch the 2026-07-05 defect"
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

    /// Audit M5 — a fresh live session defaults DEC 1007 (alternate scroll) ON, and
    /// an app can still turn it off with ?1007l.
    #[test]
    fn alternate_scroll_defaults_on_for_a_live_session() {
        let t = new_live_terminal(10, 40, None, aterm_types::Appearance::Dark);
        assert!(
            t.modes().alternate_scroll,
            "DEC 1007 must default ON for a live session"
        );
        let mut t2 = new_live_terminal(10, 40, None, aterm_types::Appearance::Dark);
        t2.process(b"\x1b[?1007l");
        assert!(
            !t2.modes().alternate_scroll,
            "an app can still turn alternate scroll off"
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
        let t = new_live_terminal(10, 40, None, aterm_types::Appearance::Dark);
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
        let t = new_live_terminal(10, 40, Some(&tc), aterm_types::Appearance::Dark);
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
        let t = new_live_terminal(10, 40, Some(&tc), aterm_types::Appearance::Dark);
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

    /// SCROLL-1 (retention) — the fix's whole point: with the store attached, history
    /// past the 10k grid ring is RETAINED (it tiers into the store) instead of being
    /// dropped. Feed well past the ring and confirm the total scrollback line count
    /// exceeds the old hard 10k cap. (Before the fix `grid.scrollback` was `None`, so
    /// lines scrolled off the ring were discarded and this count could never pass 10k.)
    #[test]
    fn live_session_retains_scrollback_past_the_10k_ring() {
        let mut t = new_live_terminal(10, 40, None, aterm_types::Appearance::Dark);
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
        let dark = new_live_terminal(10, 40, None, Appearance::Dark);
        assert_eq!(dark.color_scheme(), Appearance::Dark);
        // Light desktop: the fresh engine must REPORT light, not the dark default.
        let mut light = new_live_terminal(10, 40, None, Appearance::Light);
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
        configure_kitty_file_transfer(&term);
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
        term_lock(&term).process(&seq);
        let placed = !term_lock(&term).cell_frame(24, 80).images[0].is_empty();

        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            placed,
            "t=f must read the real file via the resolver and place the image"
        );
    }

    #[test]
    fn temp_file_medium_is_consumed_then_deleted() {
        let dir = std::env::temp_dir().join(format!("aterm-kft-t-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("temp.rgba");
        std::fs::File::create(&path)
            .unwrap()
            .write_all(&vec![0u8; 10 * 20 * 4])
            .unwrap();

        let term = term_with_resolver();
        let seq = apc("a=T,f=32,s=10,v=20,t=t", path.to_str().unwrap().as_bytes());
        term_lock(&term).process(&seq);

        let placed = !term_lock(&term).cell_frame(24, 80).images[0].is_empty();
        let deleted = !path.exists();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(placed, "t=t must consume the temp file + place the image");
        assert!(deleted, "t=t must DELETE the temp file after reading it");
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
        term_lock(&term).process(&seq);
        let placed = !term_lock(&term).cell_frame(24, 80).images[0].is_empty();

        let _ = std::fs::remove_dir_all(&dir);
        assert!(!placed, "an over-cap file must be rejected (fail closed)");
    }
}
