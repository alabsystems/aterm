// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **aterm-agent** — layer L2 of the RFC "The Reactive Surface": the *agent
//! interface*. Two responsibilities, both of which MUST live outside the engine
//! core (RFC R2):
//!
//! 1. **Turn-completion** ([`Turn`]). "An agent finished its turn" is the most
//!    semantic thing in the stack — it is `IdleFor(d) ∧ RowMatches(prompt-ready)`
//!    composed over the L0/L0.5 predicates, plus response-region extraction and
//!    the Claude-specific prompt-ready patterns. None of this belongs in the
//!    terminal; it lives here, two crates above `aterm-core`.
//!
//! 2. **The self-reflection feedback governor** ([`SelfGovernor`]). When the
//!    observer and the observed are the *same* session (R4 self-reflection),
//!    `await-idle` alone does **not** damp the loop — a self-write that produces
//!    output keeps `content_seq` advancing. The governor is the safety bound:
//!    self-writes are **off by default**, rate-limited by a token bucket, and a
//!    circuit-breaker trips on sustained self-induced churn. Its `FailClosed`
//!    invariant is model-checked by `self_governor_model` (`aterm-spec`) and
//!    bound to this code by [`tests`].
//!
//! > **Layering note (the critic's gap).** This L2 governor is *policy*. The
//! > *un-bypassable floor* — a hard per-session rate-limit on self-targeted input
//! > injection — lives at the control dispatch path (`aterm-gui::inject_floor`,
//! > applied in `control.rs`/`run_feed_bin`), because a raw control client can
//! > drive `@.` in a loop without ever linking this crate. (Cross-session
//! > self-amplification is separately bounded by the proxy's per-op edge tokens,
//! > whose `DeriveLoop` op is un-grantable by default — `ProxyEntry::token_for`
//! > returns `None` for it.) This crate is the rich policy on top of that floor,
//! > not a substitute for it.

/// Durable, thread-safe state machine for the embedded fleet operator.
pub mod operator;

use std::io::{Read, Write};
use std::time::Duration;

use aterm_observe::row_matcher;

/// The self-reflection feedback governor (R4). A bounded state machine whose
/// `FailClosed` property — *a self-write is permitted only with a spare token, a
/// non-tripped breaker, and self-writes explicitly enabled* — is model-checked.
#[derive(Clone, Debug)]
pub struct SelfGovernor {
    /// Self-driving is OFF unless the operator explicitly enables it.
    self_write_enabled: bool,
    /// Token bucket: available write permits.
    tokens: u32,
    /// Bucket capacity (also the refill ceiling).
    capacity: u32,
    /// Permits restored per [`tick`](Self::tick).
    refill: u32,
    /// Accumulated self-induced output since the last decay.
    churn: u32,
    /// Trip threshold: churn above this trips the breaker.
    churn_trip: u32,
    /// Once tripped, all self-writes are refused until [`reset`](Self::reset).
    tripped: bool,
}

impl SelfGovernor {
    /// A governor with self-writes **disabled** (the default posture). Capacity
    /// `capacity` permits, refilling `refill` per tick, tripping the breaker once
    /// self-induced churn exceeds `churn_trip`.
    #[must_use]
    pub fn disabled(capacity: u32, refill: u32, churn_trip: u32) -> Self {
        Self {
            self_write_enabled: false,
            tokens: capacity,
            capacity,
            refill,
            churn: 0,
            churn_trip,
            tripped: false,
        }
    }

    /// Explicitly opt into self-driving (the operator's deliberate choice). Even
    /// then, every write still passes the token bucket and the breaker.
    pub fn enable_self_write(&mut self) {
        self.self_write_enabled = true;
    }

    /// May a self-write proceed *right now*? Consumes one token on success. This
    /// is the FailClosed gate: `false` unless self-writes are enabled **and** the
    /// breaker is not tripped **and** a token is available.
    #[must_use]
    pub fn allow_self_write(&mut self) -> bool {
        if !self.self_write_enabled || self.tripped || self.tokens == 0 {
            return false;
        }
        self.tokens -= 1;
        true
    }

    /// Record `amount` of self-induced output. Sustained churn trips the breaker
    /// (latching) — the storm backstop that `await-idle` alone cannot provide.
    pub fn note_self_output(&mut self, amount: u32) {
        self.churn = self.churn.saturating_add(amount);
        if self.churn > self.churn_trip {
            self.tripped = true;
        }
    }

    /// One governor tick: refill the bucket (capped) and decay the churn window.
    pub fn tick(&mut self) {
        self.tokens = self.tokens.saturating_add(self.refill).min(self.capacity);
        self.churn = self.churn.saturating_sub(self.refill);
    }

    /// Whether the breaker has tripped (manual [`reset`](Self::reset) to recover).
    #[must_use]
    pub fn tripped(&self) -> bool {
        self.tripped
    }

    /// Operator recovery after a trip: clear the breaker and refill.
    pub fn reset(&mut self) {
        self.tripped = false;
        self.churn = 0;
        self.tokens = self.capacity;
    }
}

/// The Claude-prompt-ready signal: an input caret (`❯`) on some row. It is a
/// row match and says NOTHING about a spinner — the glyph stays on screen while
/// Claude thinks, which is why [`DRIVE_HELP`] recommends `await gone` on the
/// busy footer. These patterns are Claude-specific and live ONLY here.
#[must_use]
pub fn claude_prompt_ready_pattern() -> &'static str {
    // The input caret at the start of a row; tolerant of the box border glyphs.
    r"(^|\s)❯(\s|$)"
}

/// A driven turn: type a prompt, submit it, then block until the agent's turn
/// completes — the surface goes `idle for `[`idle`], then a best-effort
/// prompt-ready confirm — and read the settled surface. The [`ControlClient`]
/// abstracts the transport (a Unix socket today, an astream network dial under
/// L3); this composition is the same regardless, and is exactly what the
/// `aterm-drive` CLI runs (this run-loop over [`CtlClient`] + the core `await`).
pub struct Turn {
    /// Quiescence window that counts as "the agent stopped streaming".
    pub idle: Duration,
    /// Overall deadline before giving up.
    pub timeout: Duration,
    /// The prompt-ready regex (defaults to [`claude_prompt_ready_pattern`]).
    pub ready_pattern: String,
}

impl Default for Turn {
    fn default() -> Self {
        Self {
            idle: Duration::from_millis(600),
            timeout: Duration::from_secs(180),
            ready_pattern: claude_prompt_ready_pattern().to_string(),
        }
    }
}

/// The transport seam the agent layer drives. Implemented over `aterm-ctl`'s
/// verbs locally and (L3) over an astream network dial remotely — the [`Turn`]
/// composition is identical either way.
pub trait ControlClient {
    /// The transport's error type.
    type Error;
    /// Type bytes into the target's input (the `send` verb).
    fn send(&mut self, bytes: &[u8]) -> Result<(), Self::Error>;
    /// Submit with a real Enter keypress (the `key enter` verb — never a raw LF).
    fn key_enter(&mut self) -> Result<(), Self::Error>;
    /// Settle the surface (`await idle`), then best-effort confirm a prompt-ready
    /// row (`await match <ready>`), then return the settled surface text. Idle is
    /// the authoritative turn-complete signal; the ready match only sharpens it.
    /// `ready_pattern` empty = idle only.
    fn await_idle_and_ready(
        &mut self,
        idle: Duration,
        ready_pattern: &str,
        timeout: Duration,
    ) -> Result<String, Self::Error>;
}

/// Why a turn could not be driven. The `Display` messages are written for an AI
/// agent reading them in a tool result — each says what happened AND what to try
/// next, so the model can self-correct without external docs.
#[derive(Debug)]
pub enum TurnError<E> {
    /// The self-reflection governor refused the write (off / rate-limited /
    /// breaker tripped).
    Governed,
    /// The supplied `ready_pattern` did not compile as a regex.
    BadPattern(regex_error::Error),
    /// The transport failed.
    Transport(E),
}

impl<E: std::fmt::Display> std::fmt::Display for TurnError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TurnError::Governed => write!(
                f,
                "self-reflection governor refused the write. This session is \
                 driving ITSELF and self-writes are off, the write budget (token \
                 bucket) is spent, or the churn breaker tripped. Fix: enable \
                 self-writes deliberately (SelfGovernor::\
                 enable_self_write) and pace the loop — act only on a settled turn, \
                 never on every output burst."
            ),
            TurnError::BadPattern(e) => write!(
                f,
                "the prompt-ready pattern is not a valid regex ({e}). Fix: pass a \
                 simple anchored pattern, e.g. '❯' for a Claude input box or \
                 '\\$ $' for a shell prompt."
            ),
            TurnError::Transport(e) => write!(
                f,
                "the control transport failed ({e}). Fix: check the target aterm is \
                 running and ATERM_CONTROL_SOCK points at its socket (the path it \
                 printed as 'control socket listening at ...')."
            ),
        }
    }
}

/// The `aterm drive` CLI (binary-era `aterm-drive`), callable in-process.
pub mod drive_cli;
/// The `aterm fleet` CLI (binary-era `aterm-fleet`), callable in-process.
pub mod fleet_cli;
/// The supervisor: read-only classification, prompt parsing, the worker's phase,
/// and the `await-turn` / `supervise` / `watch` loop behind `aterm drive`.
pub mod supervise;

/// Re-export so callers can match on a compile failure without depending on
/// `regex` directly (it is validated through `aterm-observe`).
pub mod regex_error {
    pub use ::aterm_observe::regex_compile_error::Error;
}

impl Turn {
    /// Drive one turn through `client`, gated by `gov` (the self-reflection
    /// governor — pass a permissive one for cross-session driving). Returns the
    /// settled surface text on completion.
    ///
    /// # Errors
    /// - [`TurnError::Governed`] if the governor refuses the write.
    /// - [`TurnError::BadPattern`] if the ready pattern is invalid.
    /// - [`TurnError::Transport`] on a transport failure.
    pub fn run<C: ControlClient>(
        &self,
        client: &mut C,
        gov: &mut SelfGovernor,
        prompt: &[u8],
    ) -> Result<String, TurnError<C::Error>> {
        // Validate the predicate before touching the transport.
        row_matcher(&self.ready_pattern).map_err(TurnError::BadPattern)?;
        // The self-reflection floor: refuse if the governor says so.
        if !gov.allow_self_write() {
            return Err(TurnError::Governed);
        }
        client.send(prompt).map_err(TurnError::Transport)?;
        client.key_enter().map_err(TurnError::Transport)?;
        let screen = client
            .await_idle_and_ready(self.idle, &self.ready_pattern, self.timeout)
            .map_err(TurnError::Transport)?;
        // Account the response toward the churn breaker (self-reflection safety).
        gov.note_self_output(u32::try_from(screen.len()).unwrap_or(u32::MAX));
        Ok(screen)
    }
}

/// A concrete [`ControlClient`] that drives a target aterm by shelling out to the
/// std-only `aterm-ctl` core client — the agent layer reuses the exact verbs a
/// human would type, with zero protocol re-implementation. Composition (idle,
/// then a bounded prompt-ready confirm, then read) lives HERE in the sugar, so
/// the core `await` verb stays single-predicate.
pub struct CtlClient {
    ctl: std::path::PathBuf,
    socket: Option<String>,
    /// The `aterm-ctl` in flight, by pid, for [`CtlClient::cutter`].
    running: std::sync::Arc<std::sync::Mutex<Option<u32>>>,
    /// Set by the cutter: every request from then on is cut short.
    cut: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl CtlClient {
    /// Build a client. `ctl` is the path to `aterm-ctl`; `socket` is an explicit
    /// `--sock` path, or `None` to use `$ATERM_CONTROL_SOCK` / the default.
    pub fn new(ctl: impl Into<std::path::PathBuf>, socket: Option<String>) -> Self {
        Self {
            ctl: ctl.into(),
            socket,
            running: std::sync::Arc::default(),
            cut: std::sync::Arc::default(),
        }
    }

    /// A handle that cuts the request in flight short from another thread —
    /// `SIGTERM` to the `aterm-ctl` running it — and every request after it:
    /// the client is DONE once cut. This is what ends the mail lane's parked
    /// `await inbox` (20 s a step) the moment its loop has its result
    /// ([`supervise::Ctl::interrupter`]). Unix only: `None` elsewhere, and
    /// the loop then ends within one step.
    #[must_use]
    pub fn cutter(&self) -> Option<supervise::Interrupter> {
        #[cfg(unix)]
        {
            let running = std::sync::Arc::clone(&self.running);
            let cut = std::sync::Arc::clone(&self.cut);
            Some(Box::new(move || {
                cut.store(true, std::sync::atomic::Ordering::SeqCst);
                let pid = running.lock().unwrap_or_else(|p| p.into_inner()).take();
                if let Some(pid) = pid {
                    // SAFETY: a plain `kill(2)` on a pid this client spawned and
                    // has not yet reaped (`running` is cleared after the wait,
                    // under the same lock), so the pid cannot have been reused.
                    unsafe {
                        libc::kill(libc::pid_t::try_from(pid).unwrap_or(0), libc::SIGTERM);
                    }
                }
            }))
        }
        #[cfg(not(unix))]
        {
            None
        }
    }

    /// Run `aterm-ctl [--sock S] <args...>`, returning stdout or a trimmed stderr.
    pub fn run(&self, args: &[&str]) -> Result<String, String> {
        let reply = self.run_raw(args)?;
        if reply.code == 0 {
            Ok(reply.stdout)
        } else {
            Err(reply.stderr.trim().to_string())
        }
    }

    /// Run `aterm-ctl [--sock S] <args...>` and return the exit code with both
    /// streams — the supervisor loop needs to tell a `124` timeout and an `ERR
    /// usage` (an older host) apart from a failure. `Err` only when the client
    /// could not be launched at all.
    pub fn run_raw(&self, args: &[&str]) -> Result<supervise::CtlReply, String> {
        use std::sync::atomic::Ordering;
        let mut cmd = std::process::Command::new(&self.ctl);
        if let Some(s) = &self.socket {
            cmd.arg("--sock").arg(s);
        }
        cmd.args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("could not run {}: {e}", self.ctl.display()))?;
        // Registered under the lock the cutter takes, and checked after it:
        // a cut that came between the spawn and the registration still
        // ends this request, not the next one.
        {
            let mut running = self.running.lock().unwrap_or_else(|p| p.into_inner());
            if self.cut.load(Ordering::SeqCst) {
                let _ = child.kill();
            } else {
                *running = Some(child.id());
            }
        }
        let out = child.wait_with_output();
        self.running
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        let out = out.map_err(|e| format!("could not run {}: {e}", self.ctl.display()))?;
        Ok(supervise::CtlReply {
            code: out.status.code().unwrap_or(1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

impl ControlClient for CtlClient {
    type Error = String;
    fn send(&mut self, bytes: &[u8]) -> Result<(), Self::Error> {
        let s = String::from_utf8_lossy(bytes);
        self.run(&["send", s.as_ref()]).map(|_| ())
    }
    fn key_enter(&mut self) -> Result<(), Self::Error> {
        self.run(&["key", "enter"]).map(|_| ())
    }
    fn await_idle_and_ready(
        &mut self,
        idle: Duration,
        ready_pattern: &str,
        timeout: Duration,
    ) -> Result<String, Self::Error> {
        let idle_ms = idle.as_millis().to_string();
        let to_ms = timeout.as_millis().to_string();
        // (1) Wait for the surface to settle — the core single-predicate
        //     `await idle` verb (turn-complete for a streaming TUI like Claude,
        //     whose spinner keeps the screen changing until the turn ends).
        self.run(&["await", "idle", &idle_ms, "timeout", &to_ms])?;
        // (2) Best-effort, advisory confirm that a prompt-ready row is present
        //     (`await match`). The surface is ALREADY idle, so a matching row — if
        //     present — returns at ONCE (free for a ready Claude prompt); a SHORT
        //     250 ms bound means a non-matching pattern (e.g. the Claude `❯`
        //     default against a `$` shell) costs at most 250 ms, never the full
        //     timeout. Non-fatal: idle is the authoritative turn-complete signal;
        //     this only sharpens it. Skipped when no pattern is set.
        if !ready_pattern.is_empty() {
            let _ = self.run(&["await", "match", ready_pattern, "timeout", "250"]);
        }
        // (3) Read the settled surface.
        self.run(&["text"])
    }
}

/// Max body lines a single `text` reply may contain — a DoS bound on an untrusted
/// `OK <n>` count (the server caps its own output; the client must not trust an
/// unbounded count from a compromised relay peer).
const MAX_TEXT_LINES: usize = 200_000;
/// Max bytes in a single control-protocol line.
const MAX_LINE_BYTES: usize = 1 << 20; // 1 MiB

/// A [`ControlClient`] over ONE persistent, already-authenticated control
/// connection — the remote twin of [`CtlClient`]. Where `CtlClient` shells out to
/// `aterm-ctl` once per verb (a fresh socket each time), `RelayClient` holds a
/// single connection and speaks the raw control verbs directly, so it can sit
/// behind a `dial <name>` relay — which bridges ONE connection for its lifetime, so
/// a per-verb shell-out never could.
///
/// It is generic over any `Read + Write` transport: a local `CtlStream` (via
/// [`connect_local`](RelayClient::connect_local)), the same after a `dial` relay
/// (via [`dial_via_local`](RelayClient::dial_via_local)), or a TLS stream in tests.
/// The verbs and their framing are **byte-identical** to what `CtlClient` drives
/// through `aterm-ctl`, so [`Turn::run`] behaves the same either way — the
/// "identical either way" promise made literal. Predicates (`await idle`/`match`)
/// run on the authoritative remote host, never on a local fold.
pub struct RelayClient<S: Read + Write> {
    io: S,
    /// Bytes read past the last consumed line boundary (the next reply's prefix).
    buf: Vec<u8>,
}

impl<S: Read + Write> RelayClient<S> {
    /// Wrap an already-connected, already-authenticated transport. Callers that need
    /// the local-socket AUTH (and optional `dial`) handshake use
    /// [`connect_local`](RelayClient::connect_local) /
    /// [`dial_via_local`](RelayClient::dial_via_local) instead.
    pub fn new(io: S) -> Self {
        Self {
            io,
            buf: Vec::new(),
        }
    }

    /// Read one `\n`-terminated line, returned without its trailing `\r?\n`.
    fn read_line(&mut self) -> std::io::Result<String> {
        loop {
            if let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
                let mut line: Vec<u8> = self.buf.drain(..=pos).collect();
                line.pop(); // drop '\n'
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return String::from_utf8(line).map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, "non-utf8 control line")
                });
            }
            if self.buf.len() > MAX_LINE_BYTES {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "control line exceeds the length bound",
                ));
            }
            let mut tmp = [0u8; 8192];
            let n = self.io.read(&mut tmp)?;
            if n == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "control connection closed",
                ));
            }
            self.buf.extend_from_slice(&tmp[..n]);
        }
    }

    /// Write one request line (`line` + `\n`) and flush.
    fn write_line(&mut self, line: &str) -> std::io::Result<()> {
        if line.contains(['\n', '\r']) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "control request must not contain an embedded line terminator",
            ));
        }
        self.io.write_all(line.as_bytes())?;
        self.io.write_all(b"\n")?;
        self.io.flush()
    }

    /// Issue a request whose reply is a single status line; `ERR …`/unexpected → Err.
    fn request_status(&mut self, line: &str) -> std::io::Result<String> {
        self.write_line(line)?;
        let resp = self.read_line()?;
        if resp == "OK" || resp.starts_with("OK ") {
            Ok(resp)
        } else {
            Err(std::io::Error::other(resp))
        }
    }

    /// Issue `text` and parse the streaming reply (`OK <n>\n` then `n` body lines).
    /// Each body line is returned with a trailing `\n`, byte-identical to the
    /// `aterm-ctl` stdout that [`CtlClient`] captures for `text`.
    fn read_text(&mut self) -> std::io::Result<String> {
        self.write_line("text")?;
        let header = self.read_line()?;
        if !(header == "OK" || header.starts_with("OK ")) {
            return Err(std::io::Error::other(header));
        }
        let count: usize = header
            .strip_prefix("OK ")
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|tok| tok.parse().ok())
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("malformed text header: {header:?}"),
                )
            })?;
        if count > MAX_TEXT_LINES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "text reply line count exceeds the bound",
            ));
        }
        let mut out = String::new();
        for _ in 0..count {
            out.push_str(&self.read_line()?);
            out.push('\n');
        }
        Ok(out)
    }
}

impl RelayClient<aterm_uds::CtlStream> {
    /// Connect to a LOCAL control socket and authenticate: `connect(sock)` then a
    /// bare `AUTH <token>\n` line (the server acknowledges it silently — no reply —
    /// then reads the first verb). No `dial`: drives the local instance directly
    /// over one persistent connection.
    ///
    /// # Errors
    /// I/O errors connecting to `sock_path` or writing the auth line.
    pub fn connect_local(sock_path: &str, token: &str) -> std::io::Result<Self> {
        let stream = aterm_uds::CtlStream::connect(sock_path)?;
        let mut client = Self::new(stream);
        client.write_line(&format!("AUTH {token}"))?;
        Ok(client)
    }

    /// Connect + authenticate to the LOCAL socket, then `dial <connection>` so the
    /// connection becomes a transparent relay to a saved REMOTE aterm's own control
    /// socket. On success the local server relays silently (writes nothing), so we
    /// probe with `version` — answered by the REMOTE — to confirm the relay is live
    /// end-to-end; a local `ERR dial …` surfaces as a clean construction error.
    ///
    /// # Errors
    /// I/O errors, or a non-`OK` reply to the post-`dial` `version` probe (the relay
    /// did not come up — e.g. the connection is unknown or the remote is unreachable).
    pub fn dial_via_local(sock_path: &str, token: &str, connection: &str) -> std::io::Result<Self> {
        let mut client = Self::connect_local(sock_path, token)?;
        client.write_line(&format!("dial {connection}"))?;
        client.write_line("version")?;
        let resp = client.read_line()?;
        if resp.starts_with("OK") {
            Ok(client)
        } else {
            Err(std::io::Error::other(format!(
                "dial {connection} failed: {resp}"
            )))
        }
    }
}

impl<S: Read + Write> ControlClient for RelayClient<S> {
    type Error = std::io::Error;

    fn send(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        let text = String::from_utf8_lossy(bytes);
        // Reject embedded newline/CR: a multi-line payload would inject a SECOND
        // authenticated control verb (mirrors aterm-ctl's validate_request_parts,
        // the contract CtlClient inherits from the argv boundary).
        if text.contains('\n') || text.contains('\r') {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "send payload must not contain a newline/CR (would inject a second control verb)",
            ));
        }
        self.request_status(&format!("send {text}")).map(|_| ())
    }

    fn key_enter(&mut self) -> std::io::Result<()> {
        self.request_status("key enter").map(|_| ())
    }

    fn await_idle_and_ready(
        &mut self,
        idle: Duration,
        ready_pattern: &str,
        timeout: Duration,
    ) -> std::io::Result<String> {
        let idle_ms = idle.as_millis();
        let to_ms = timeout.as_millis();
        // (1) Authoritative settle — the remote's own `await idle` on its WatcherSet.
        self.request_status(&format!("await idle {idle_ms} timeout {to_ms}"))?;
        // (2) Best-effort prompt-ready confirm. The reply is DISCARDED, but MUST be
        //     consumed before `text` or the persistent stream desyncs (the one
        //     non-obvious correctness point vs the shell-out CtlClient).
        if !ready_pattern.is_empty() {
            self.write_line(&format!("await match {ready_pattern} timeout 250"))?;
            let _ = self.read_line()?;
        }
        // (3) Read the settled surface.
        self.read_text()
    }
}

/// The AI-oriented help for the `aterm-drive` tool — written so a model reading
/// `--help` in a tool result builds correct intuition for the CORE primitives
/// (the `await`/`send`/`key`/`text` verbs) and the drive loop, without external
/// docs. The core protocol stays terse; this is where the teaching lives.
pub const DRIVE_HELP: &str = "\
aterm-drive — drive an interactive agent (e.g. Claude Code) running inside aterm.

MENTAL MODEL
    A HOST aterm runs your target program as its child and exposes a Unix control
    socket. This tool reads the live screen and drives keystrokes with the same
    control verbs exposed by `aterm-ctl`. The key primitive is
    `await`: block until the surface reaches a condition, so you never sleep-and-
    hope or scrape for a spinner.

USAGE
    aterm-drive [--socket PATH] [--idle MS] [--timeout MS] [--ready REGEX]
                <command> [text...]
    aterm-drive classify [--allow-python GLOB]... <cmd...> | phase [@sid]
              | await-turn [@sid] [--timeout MS] [--reconnect-s S]
              | supervise [@sid] [--auto-reads] [--max-s S] [--allow-python GLOB]... [--notes FILE]
                          [--reconnect-s S] [--dismiss-surveys] [--context-warn PCT]
                          [--journal FILE] [--mail [--inbox @sid] [--report-window S] [--idle-grace S]]
              | watch [@sid] [--auto-reads] [--allow-python GLOB]... [--notes FILE] [--max-s S]
                      [--reconnect-s S] [--report] [--dismiss-surveys] [--context-warn PCT]
                      [--journal FILE] [--mail [--inbox @sid] [--report-window S] [--idle-grace S]]
                      [--resume [RULES]]
              | task @sid [--deadline S] [--wait] [--no-nudge] [--inbox @sid] <text...>
              | report [@sid] [--since ORIGIN:I] [--max-rows N] [--final | --messages]
              | ledger [@sid] [--journal FILE] [--since TIME] [--format text|md|html] [--out PATH]

COMMANDS
    prompt <text...>   Type <text>, press Enter, then BLOCK until the agent's turn
                       settles (no screen change for --idle ms), and print the
                       settled screen. This is the one you want for a drive loop.
    read               Print the live screen (one row per line).
    await <cond>       Block until a condition, then print the kernel's verdict:
                         idle <ms>        surface unchanged for <ms> (turn done)
                         match <regex>    a visible row matches <regex>
                         gone <regex>     NO visible row matches <regex> (a busy
                                          footer such as 'esc to interrupt' left)
                         seq              the next content change lands
                         block            a shell command completes (OSC-133)
    shot [name.png]    Save a pixel-true PNG of the terminal content view (the
                       rendered cells; OS chrome/titlebar are NOT captured). The
                       name is a BARE filename: captures land in the host's
                       Application Support images/ dir (a '/' is refused) and
                       the reply prints the full written path; auto-named when
                       omitted.
    help               Show this text.

SUPERVISING A WORKER (a Claude Code session in another tab; `@sid` from `aterm ctl ls`)
    classify [--allow-python GLOB]... <cmd...>
                       Is this shell line READ-ONLY, the way the supervisor judges
                       it? Prints `read-only` (exit 0) or `not-read-only <reason>`
                       (exit 1). The flags come first; from the command's first
                       word on, every word is the command's (`classify git log
                       --oneline` works unquoted; `--` also ends the flags). Quoted
                       strings are dropped before the danger scan, every danger
                       token anywhere fails it (rm mv cp tee … git push/reset/
                       commit … a redirect to a file, sed -i, python3 -c, sort -o,
                       uniq IN OUT, find -fprint), and every segment's head must be
                       a known read-only program — a wrapper is seen through
                       (`xargs touch`, `env FOO=1 ./x.sh` are touch and x.sh), a
                       `&` ends a segment like `;`, git needs a read-only
                       subcommand and form (`git branch NAME` creates), python3
                       only a script on the --allow-python globs (default
                       scripts/*standing*.py, scripts/*report*.py,
                       scripts/*score*.py — all under scripts/; no `..`).
                       The programs handed to awk and sed are read from the raw
                       words: `system(`, a redirect or a pipe in awk, a `w`/`e`
                       command or `s///w` flag in sed, a `-f` program file, all
                       refuse. A tie breaks toward not-read-only.
    phase [@sid]       One read, one word: busy | prompt | limited | idle |
                       question. For a prompt the parsed box follows: `kind`,
                       `command`, `description`, `classify` (a Bash box), one
                       `option N …` per option, then `cancel esc` or `cancel
                       none`. For busy, `reason <where>: <rule>` names the signal
                       that fired. For limited, `message <text>` and `reset
                       <text|->`. A prompt wins over busy, busy over limited,
                       limited over question — except a busy that is only a
                       background monitor, which a limit notice or a question
                       outranks (a persistent monitor can outlive any budget).
                       Busy is read from the LIVE ZONE around Claude Code's
                       composer, which sits between two full-width rules. The
                       STATUS ROW is, walking up from the top rule, the first row
                       that starts in column 0 with a spinner glyph, found before
                       any transcript row (the worker's `⏺` message or tool call,
                       output under the `⎿` gutter); what sits between it and the
                       rule never hides it (a tip, a todo list, a hint of any
                       length, the session survey, a banner, a queued message).
                       It is busy when it is a spinner running into an ellipsis,
                       `Waiting for N …` (a dynamic workflow, a background agent)
                       or a done row still counting `N shell(s) still running`;
                       so is a `Still working` hint under it, and the FOOTER
                       under the bottom rule: `esc to interrupt`, `Still
                       working`, `ctrl+b to run in background`, `· N shell(s) ·`,
                       a workflow's `◯ … agents done` line. A monitor still
                       running (`N monitor(s) still running` on the status row,
                       `· N monitor(s) ·` in the footer) is the soft busy above.
                       A status row above a transcript row is history, not a
                       signal. With no composer rules on the screen every row is
                       scanned. The footer does not always say `esc to interrupt`
                       while a turn runs, so the status row is the first signal.
                       Limited is the usage-limit wall: nothing busy (a monitor
                       aside), no box, and the last thing said above the
                       composer (the done row, hints, tips, the survey and
                       banners under it skipped) is a block under the `⎿`
                       gutter that OPENS with a limit notice — `You've reached
                       your … limit`, `You've hit your … limit`, a few words and
                       `limit reached`, or `API Error` with a rate or usage
                       limit — or the footer carries one. Anything
                       said after a notice makes it history: a later turn (a
                       background command's end or a monitor event starts one
                       too), the `/model` output. The worker's own words about
                       limits, the rows your message wrapped onto and a tool's
                       output it went on to discuss never count, and neither does
                       text that says `Approaching` a limit. The worker sits at an
                       idle composer, and what you send it fails until the limit
                       resets or its model is switched.
                       A last line `survey 0` follows when Claude Code's session
                       survey is OPEN above the composer — `● How is Claude doing
                       this session?` in column 0 over `1: Bad … 0: Dismiss`,
                       only blank rows, a right-aligned hint or a tip between it
                       and the top rule (a copy quoted in the transcript is not
                       open). `0` is the key that dismisses it: `aterm ctl @sid
                       key 'if=^●.How.is.Claude.doing' 0` — the guard matches
                       only a row that starts with `●`, so a copy quoted in the
                       transcript (indented, or under `⎿`) lets no `0` through
                       to the composer (quoted: `^` is a glob in zsh with
                       extended_glob). While it is open, a turn whose first
                       character is 1, 2 or 3 is taken as a rating — the human's
                       to give, never yours. Dismiss it first.
                       A last line `context <n>%` follows (after any `survey 0`)
                       while Claude Code's context indicator is up: `<n>% until
                       auto-compact` (or `Context left until auto-compact: <n>%`)
                       under the status row (with none, under the last transcript
                       row) and above the top rule, alone on its row, ending
                       against the right edge where the rules end (in a narrow
                       pane too). A copy quoted in the transcript or in a tool's
                       output does not count unless it, too, ends at that edge.
                       That much of the worker's context is left before Claude
                       Code auto-compacts it, replacing its history with a summary
                       in which the rules you gave it may not survive (see
                       --context-warn).
    await-turn [@sid] [--timeout MS] [--reconnect-s S]
                       Block until the phase is no longer busy, then print it
                       exactly like `phase` (its `survey 0` and `context <n>%`
                       lines included). The loop is `await idle 2000` → read →
                       `await seq` (never a sleep); where the host knows `await
                       gone`, the busy footer LEAVING is the first wait. A screen
                       without the composer rules (a build, a script, a REPL) whose
                       output never held still for the 2 s is busy too: its turn
                       ends when the output pauses. Exit 124 on --timeout (default:
                       the global --timeout) with the worker still busy — or with
                       the connection lost (see --reconnect-s): then the phase of
                       the last screen read, or `busy` with `reason no screen: no
                       read answered before the timeout` when none was.
    supervise [@sid] [--auto-reads] [--max-s S] [--allow-python GLOB]... [--notes FILE]
              [--reconnect-s S] [--dismiss-surveys] [--context-warn PCT]
                       The loop: await-turn; with --auto-reads, a Bash prompt whose
                       command classifies read-only is approved (option 1, pressed
                       GUARDED: `key if=Do.you.want.to.proceed 1` on a host that has
                       it — `OK seq=<n>` is the press, `OK skipped seq=<n>` means
                       NOTHING was pressed or approved: the box had left (the loop
                       goes on), or the seq is the screen just parsed and the
                       guard matched no row (that box is handed to you); a host
                       without the guard answers a usage line or a bare `ERR` and
                       the press falls back to read → confirm → press → re-read,
                       backspacing a digit that landed in the composer — and
                       with the session survey open on the confirming read it
                       presses nothing and hands the box to you (a `1` that
                       reaches the survey is a rating); `ERR busy sink` is
                       retried, any other `ERR` stops the loop), one line
                       appended to --notes, then `await seq` until the box has LEFT
                       before the next look (an unchanged screen is never pressed
                       twice; one that does not move after the press is handed to
                       you), and the loop continues. The same read coming back
                       after two approvals is handed over too. ANYTHING ELSE — a
                       write prompt, a workflow, a question, a limit notice, an
                       idle composer — prints the compact result (the phase lines,
                       then the prompt box or the last 28 non-blank rows) and
                       exits 0: that is YOUR review point. Once --max-s (default
                       1800) is spent it prints TIMEOUT, then the last read's
                       compact result, and exits 124 — a turn read at or after
                       the deadline is not pressed, and a budget spent while a
                       lost connection is ridden out is the TIMEOUT too. The
                       session survey is never answered: when it appears (see
                       phase) supervise says watch's `EVENT survey` line on
                       stderr, or with --dismiss-surveys presses its `0` and
                       says `DISMISSED survey seq=<n>` there once it has gone
                       (see --dismiss-surveys). The worker's context running low,
                       and the compaction after it, are said there too, as they
                       happen during the run: watch's `EVENT context` and `EVENT
                       compacted` lines (see --context-warn; a compaction between
                       two runs is not seen).
    watch [@sid] [--auto-reads] [--allow-python GLOB]... [--notes FILE] [--max-s S]
          [--reconnect-s S] [--report] [--dismiss-surveys] [--context-warn PCT]
          [--journal FILE] [--mail …] [--resume [RULES]]
                       supervise's loop for a harness that wakes its agent once per
                       stdout line (a background monitor, a supervisor process).
                       Approvals are supervise's, and each prints `APPROVED
                       seq=<n> <command>`; --notes gets supervise's lines. Each
                       decision is ALSO told to the worker's window as it is
                       printed — `aterm ctl @sid story <verb> [<text>]` through
                       a client of its own, one per journaled APPROVED,
                       DISMISSED, RECONNECTED, TIMEOUT, EXIT, `EVENT context`
                       and `EVENT compacted` line (approved dismissed
                       reconnected warned compacted timeout exit; never the
                       command, and an APPROVED is told bare) — so the presence
                       band under the window's tab bar reads `✓ approved` for
                       three seconds and its `◇ quiet` summary counts the
                       approvals; the printed lines are byte-identical with or
                       without a host that knows the verb. A
                       review point prints ONE line and the loop KEEPS WATCHING:
                         EVENT <phase> seq=<n> <summary>
                       the summary being, for a prompt, `kind=<k> classify=
                       <read-only|not-read-only:<reason>|-> command=<command>` (`-`
                       for a box that is not Bash; a workflow's description stands
                       in for its command); for a question or idle, the last row
                       the worker said; for limited, `message=<text> reset=
                       <text|->` — each cut at 160 characters. Then it waits for
                       the screen to move past that point (`await seq`) before it
                       looks again, so your `turn` or `key` is picked up by
                       itself. A point that looks like the one last reported —
                       the same phase, summary and status row, the same last 8
                       non-blank transcript rows up to it (their letters: a
                       timer that ticks is no change), the same box — is neither
                       pressed nor printed again unless, in between, a read saw
                       the worker busy, it approved a read, or an outage came
                       (see --reconnect-s: nothing could be read in it, so the
                       point still showing after it is printed once more); a new
                       box, a new reply, your own row or a retry's new notice
                       changes those rows, however short the busy spell before
                       it. The two-approvals count restarts at every review
                       point, as a fresh supervise's would. A worker without the
                       composer rules is looked at when its output pauses, as
                       await-turn waits. Every line is flushed. The last line is
                       TIMEOUT (exit 124) once --max-s (default 1800) is spent —
                       no press and no EVENT comes after the deadline, and a
                       budget spent in an outage is the TIMEOUT too — or `EXIT
                       <reason>` (exit 1): the session ended (`EXIT session gone
                       (…)`, seen within one 20 s wait), an outage outlasted its
                       window (`EXIT reconnect window lapsed: …`, see
                       --reconnect-s), a request or the notes file failed, or,
                       before the loop ran, one of its flags or the host (also
                       on stderr). With --report, an idle, question or limited
                       point's line carries `complete=<0|1> rows=<n>` of
                       `report` (read at that point) between the seq and the
                       summary: `EVENT idle seq=<n> complete=1 rows=57 <summary>`;
                       run `report` to read the rows. Without it the line is as
                       above. When the session survey APPEARS (open at this look,
                       and seen gone since it was last said — by any read, a
                       busy one included; see phase) one line comes ahead of
                       that look's point, and none again while it stays open:
                         EVENT survey seq=<n> dismiss with: <command>
                       the command being `aterm ctl @sid key
                       'if=^●.How.is.Claude.doing' 0` (`aterm ctl key …` with no
                       @sid given). While a prompt box is up, or text is typed
                       in the composer (on any of its rows), the survey waits:
                       the box is reported or approved first, and the survey
                       line comes at the next look without them. With
                       --dismiss-surveys the loop presses that `0` itself and
                       prints `DISMISSED survey seq=<n>` instead once a fresh
                       read shows the survey gone (see --dismiss-surveys). As
                       the worker's context runs low it prints, at the first
                       read at or below --context-warn (mid-turn too, ahead of
                       any point) and once a descent:
                         EVENT context seq=<n> <v>% until auto-compact
                       and once the indicator has gone again (or jumped 30
                       points or more), the worker having compacted:
                         EVENT compacted seq=<n>
                       (see --context-warn). With no indicator on the screen,
                       every line is as above. With --mail (see there) the
                       worker's end-of-turn report comes by mail and its idle
                       point prints as ONE line, `EVENT turn seq=<n>
                       report=<id> rows=<n> <summary>`; without the flag every
                       line is as above, byte for byte.
                       A usage limit is a decision point the loop handles
                       ALONE (measured 2026-09-15 16:51 → 09-17 08:55: one
                       account's weekly limit stopped the manager and the
                       worker at once, the loop printed `EVENT limited`, ran
                       out its --max-s and exited; nobody could act for two
                       days). On `EVENT limited` it sets the worker's
                       attention (`meta set attention limited: <message>
                       reset=<when>`, cut at 256 bytes — the typed escalation
                       aterm's menu bar badges), posts the same text as
                       `kind=control` mail from the worker's session to yours
                       (--inbox, else $ATERM_PARENT_SESSION_ID; with neither,
                       skipped) and journals one `ESCALATED seq=<n>
                       attention=<reply> mail=<reply>` line — once a limit
                       episode: a retry's notice prints its EVENT and
                       escalates nothing again. It never exits on a limit: a
                       --max-s that would run out before the reset the notice
                       names (`resets Sep 19 at 11am (America/Los_Angeles)`,
                       `resets 7:30pm`, `resets in 3h`; the zone's offset as
                       `date` reads it today, the local zone with none; a span
                       counts from the notice's print, so a watcher started
                       onto a notice that sat reads it late — the probe's
                       backoff covers that; a reset read as more than 8 days
                       off is misread and extends nothing) plus 10 min is
                       stretched to that, `EXTEND until=<UTC> reset=<text>`
                       printed once a reset. Claude Code's auto-continue
                       notice (`⚠ Usage limit reached · continuing
                       automatically at 1:50pm · esc to cancel`, measured
                       2026-09-17; later `continuing shortly`) names that time
                       as its reset (`reset=1:50pm`; `shortly` is a minute
                       off) and STAYS on the screen while the worker resumes
                       under it — a busy status row or footer under it reads
                       busy, never limited. The episode ends when the worker
                       works again: after that notice, at the FIRST busy read
                       (not at the resumed turn's point, which a background
                       shell kept fourteen hours off the day it was measured);
                       after a notice naming a reset, when the worker answers
                       — a point after it was read busy (your turn after the
                       reset), or a box — since a retry may hit the wall
                       again. Either way the attention is cleared (`meta
                       unset attention`; on the wire `meta set attention ''`
                       is a usage error) and `CLEARED seq=<n> …` journaled;
                       a point prints as ever. The wall again after that busy
                       read, before the worker has answered (the retry's
                       spinner, then `continuing shortly`), is the same
                       episode opened again: the attention set again, no
                       second mail (`ESCALATED … mail=skipped: the retry hit
                       the wall again, the episode of seq=<m>`), the probe's
                       backoff standing. With --resume the loop PROBES
                       the worker itself: at the reset (a minute past the
                       time an auto-continue notice names: Claude Code's own
                       continuation goes first) — or sooner, when the screen
                       leaves the notice with no busy spell (the `/login` of
                       another account, the `/model` output) — ONE turn
                       (`turn idle=600 timeout=2500
                       Manager's watcher: the usage limit should have reset.
                       Answer with one line: can you work now, and what was
                       the last thing you completed?`), only at an idle
                       composer with nothing typed (a draft, or no composer on
                       the screen — the `/login` dialog's code field — defers
                       it a step, a question leaves it to you; journaled
                       `PROBE sent|deferred seq=<n> …`). Its answer — a new
                       done row, or a box; a worker still busy on it when 120
                       s are up is waited for, as any turn is — prints `EVENT
                       resumed seq=<n> <its line>` (the point is not reported
                       again as idle) and, with RULES named, the file's
                       contents are sent as ONE turn (`Manager's watcher,
                       standing rules restated after a limit: <the file, line
                       breaks as spaces>`; read at the launch and again then)
                       and `EVENT rebriefed seq=<n>` follows — `EVENT
                       rebrief-failed seq=<n> <why>` when it cannot be read or
                       the turn was not submitted; a worker that answers on
                       someone else's turn is rebriefed at that idle point the
                       same way. The notice again, a turn not submitted, or no
                       reaction within 120 s prints `EVENT still-limited
                       seq=<n> <why>` and the next probe waits 10 min, then 30
                       (never before a later reset a new notice names; the
                       notice again with the same text — a retry's, an
                       outage's re-report — moves no reset and brings no probe
                       forward). The last unfinished directive is yours to
                       resend — the journal and `history` show it; the loop
                       invents no work. Without --resume every line is as
                       above but for the one EXTEND.
    task @sid [--deadline S] [--wait] [--no-nudge] [--inbox @sid] <text...>
                       Give the worker its work BY MAIL: `post to=@sid
                       kind=task [dl=<S*1000>] <text>` from your own session
                       (@self: $ATERM_PARENT_SESSION_ID, or --inbox), so the
                       body never goes through the PTY, then — unless
                       --no-nudge — one read of the worker's screen, and ONLY
                       when its phase is idle, the one-line nudge
                         Inbox: task @<off>
                       typed as a `turn` (idle=600 timeout=2500, not waited
                       on: its verdict may say status=timeout — submitted=1 is
                       what counts) — a busy worker gets the mail alone, and
                       reads it at its next look at the inbox (a worker with
                       round 12's wake hooks installed, and your sid in their
                       --accept-from — every human is accepted, a session only
                       when listed — reads it on its next Stop, which is why
                       such a worker needs --no-nudge: the hook wakes it, and
                       a nudge typed over that is a second turn). Prints
                       `task @<off> nudged=0|1` once the post
                       LANDED (the broker's offset, which the worker's `inbox`
                       row shows as `off=` and its answer carries as `re=`).
                       A post that did not land is the error, in the server's
                       words: `queued=1` says it is in the outbox and WILL land
                       when a bridge drains it (do not re-post), `no-bridge=1`
                       that this instance has no bridge to drain it, `ERR
                       timeout id=<n>` that the landing was not seen in time
                       (queued all the same), `unroutable|ambiguous|
                       undeliverable` that the address is wrong. --wait parks
                       `await inbox` on your inbox (from the newest row id
                       read BEFORE the post, so nothing lands unseen) for an
                       `answer`, `report` or `ack` whose `re=` is that offset
                       — a row of those kinds answering something else re-arms
                       the wait — and prints its `MAIL id=<n> off=<o>
                       from=<sid> kind=<k> len=<n> re=<o>` line, then its body
                       (`inbox get`); bounded by --deadline, else the global
                       --timeout (ms) — the host clamps one wait at 600 s, so
                       a bound above it is re-armed until spent: the last line
                       is then `TIMEOUT no answer, report or ack re=<off>
                       within <S> s`, exit 124.
                       --deadline S also rides on the post as the advisory
                       `dl=` the worker's row shows.
    report [@sid] [--since ORIGIN:I] [--max-rows N]
                       What the worker said since your turn, in full. Read it
                       instead of the screen: Claude Code runs on the ALTERNATE
                       screen and repaints in place, so what scrolled off its top
                       is gone from the screen — the host keeps those rows (`aterm
                       ctl @sid offscreen`), and this joins them with the screen's.
                       Prints one header line, `--`, then the rows:
                         report complete=<0|1> [reason=<r>[,<r>...]] marker=<m>
                         turn=<id|-> rows=<n> archived=<a> screen=<s> last=<o:i|->
                       The start (marker=ledger): your newest `turn` whose submit
                       landed (the newest at all when none did; `history`); its
                       `arch=` mark says where the archive stood when it began,
                       and the report opens at the last `❯` row in column 0 whose
                       text begins with the turn's first 60 characters
                       (whitespace ignored, so a wrap never matters) — or, when
                       the turn's text is a paste (a line break, or 200+
                       characters) Claude Code shows as `[Pasted text #N +L
                       lines]`, the last such row if its L fits the text. With no
                       turn in the ledger (you typed with `prompt` or by hand, or
                       an aterm self-update could not carry the ledger): the
                       last `❯` row (marker=user-row). --since ORIGIN:I (a
                       `last=` from an earlier report) starts right after that
                       archived row (marker=since). The rows come from ONE
                       `offscreen … max=<--max-rows> screen=1` read (`since=<mark>`,
                       or `tail=` with no mark): the archived rows, then the
                       screen's, less the rows it shows again from the archive
                       that the read got and everything from the live zone down
                       (a spinner or `Waiting for …` status row and what hangs
                       under it — under a done row, only what hangs under it — or,
                       above an idle composer, the blank rows, right-aligned
                       hints, tips and survey parked there — the composer, its
                       footer), found by position; every other row is kept
                       verbatim (done rows, tables, todo items, code), blank rows
                       at either end aside. archived=/screen= count where the
                       rows came from. complete=1 only when the host kept an
                       archive, the start was found, the archive is the one the
                       mark named, the worker is on the alternate screen, no row
                       after the start was evicted and no gap lies after it, and
                       --max-rows (default 8000) held every row; otherwise
                       reason= lists why: no-archive (a host without `offscreen`
                       or with its archive off, or an `aterm ctl` too old to
                       relay the rows: the screen alone), main-screen (the worker
                       is not on the alternate screen — not a fullscreen app, or
                       it left one: its main screen's scrollback is not read),
                       archive-reset (the host restarted since the mark, or an
                       aterm self-update could not carry the archive — one that
                       could keeps it, turn ledger and all), archive-gap (rows
                       evicted, or a redraw with no overlap, a resize or a reset
                       after the start: something may be missing; a resize as a
                       self-update takes over loses nothing, rows may repeat),
                       max-rows (more rows
                       than --max-rows: some were not read; raise it),
                       marker-not-found (everything read, from the top). With a
                       start found in the archive, a gap the read counts after
                       the mark is placed against it with a one-row `offscreen`
                       read: one just before your turn (a resize) is not a gap in
                       it. Exit 0 whatever complete= says; 1 when a request fails
                       (the session gone, the host unreachable).
                       --final prints ONLY the worker's last message block —
                       from its last `⏺` message row (never a tool row: a call
                       like `⏺ Bash(` or `⏺ Workflow(`, one of Claude Code's
                       `Background command \"…\"` / `Dynamic workflow \"…\"` /
                       `Task Output` / `Stop Task` notices, a head whose `⎿`
                       output hangs on the very next row, or a collapsed
                       `Ran 3 shell commands` group, running or finished)
                       through the done row that
                       ended the turn. --messages prints every message block
                       and every `❯` row of yours, the done rows with them, and
                       no tool row or `⎿` output at all. A table, a bullet, a
                       todo item or indented code inside a message is kept, as
                       it is without a view. Both add
                       ` view=<final|messages> kept=<n>` to the header after
                       `last=`; `rows=` still counts every row the report
                       holds, and without either flag the output is byte for
                       byte what it always was. Measured 2026-09-14: a manager
                       read whole reports of 689 and 249 rows to find a final
                       message of about 70.
    ledger [@sid] [--journal FILE] [--since TIME] [--format text|md|html] [--out PATH]
                       How the loop RAN, replayed on one time axis: what you
                       sent, what the watcher decided, how big the worker's
                       replies were, and the fabric mail in between. It joins
                       four sources, and names the ones it could not read:
                         * the worker's turn ledger (`history`): every turn, its
                           start, and how the `turn` verb settled (`settled in
                           1.8s`, `timeout after 6.0s`);
                         * the size of the reply each turn drew, in rows, from
                           ONE `offscreen tail=20000 max=20000 screen=1` read
                           joined as `report` joins it — the rows from the
                           turn's `❯` row to the next turn's (a turn whose own
                           row is nowhere, queued or lost to a restart, is named
                           in the count that holds its reply);
                         * the watcher's --journal (below), when given: every
                           EVENT, APPROVED, DISMISSED, RECONNECT, TIMEOUT and
                           EXIT line, with the wall-clock time it was printed;
                         * this session's fabric mail with that worker — the
                           `inbox --peek --meta` rows from it (nothing is listed
                           or handled) and the `post` rows of this session's
                           `timeline` addressed to it.
                       SUMMARY counts the turns, the worker's busy time, your
                       response latency (median and max from each EVENT idle or
                       question to the next turn's start), approvals,
                       dismissals, context warnings and compactions,
                       reconnects, mail in and out, and how many reports were
                       complete. TIMELINE is one row per item in time order:
                       time, lane (manager | worker | watcher | fabric), what,
                       and the duration or latency. --format text (the default)
                       aligns columns, md writes tables, html writes ONE
                       self-contained page — inline style and script, nothing
                       fetched — with the manager, watcher and worker swimlanes
                       on a time axis and the fabric's mail on a fourth, turns
                       as bars, the whole line on hover, and the same rows as a
                       table under it. --out PATH writes it there, created
                       0600, and prints the path instead — the window's LEDGER
                       KEY (⇧⌘L on macOS, Ctrl+Shift+L elsewhere; `open_ledger`
                       in `[keybindings]`) runs exactly `ledger @<focused sid>
                       --format html --out <a 0600 file in the temp dir>` and
                       opens the file in the browser. --since takes Unix
                       milliseconds or a time word (`2026-09-14`,
                       `2026-09-14T10:30`, with `Z` or `±HH:MM`; a bare date or
                       time is local) and drops everything before it. With no
                       @sid the worker is the one the journal's own lines name.
                       `history`, the inbox and the timeline are stamped by the
                       aterm process's own clock, not wall time; they are
                       placed by the BIRTH TIME of that instance's control
                       socket (measured 2026-09-14: within 0.12 s of the fabric
                       bus's own stamps on the same three messages, where the
                       process's start time was 5.3 s early). Without that the
                       times are marked `~` and no latency is claimed, and a
                       turn an aterm self-update carried from an earlier
                       process (`carried=1`) is on that process's clock and has
                       no time at all. It only reads: exit 0 even when a source
                       was missing.
    --mail             (supervise, watch) Mail is your channel; the screen is
                       the safety net. The loop parks ONE `await inbox
                       since=<id>` on YOUR session (@self, or --inbox @sid)
                       from a thread of its own with a control client of its
                       own — the worker's socket sees not one request more,
                       except one read per 20 s step while an idle point is
                       held (below) — re-armed after each delivery from the
                       newest row id and after each 20 s step it runs out (no
                       polling), and prints, on stdout with every other line
                       as each row lands:
                         MAIL id=<n> off=<o> from=<sid> kind=<k> len=<n> [re=<o>]
                       (id: the row `inbox get <id>` reads; off: the bus
                       offset an answer names as re=; from: the attested
                       sender; nothing of the body — trust= is in the inbox
                       row, and the body is yours to read). The worker's
                       end-of-turn `report` (round 12's hooks post it from
                       the Stop hook, `aterm link hook install claude
                       --report-to @<you>`) is folded into the idle point of
                       the same turn: the point is HELD — nothing printed —
                       until the report lands or --idle-grace S (default 180)
                       runs out, the screen read once per 20 s step of the
                       hold as the safety net (a prompt, a question or the
                       worker busy again has superseded the point: it is said
                       as idle-no-report, and what followed right after; a
                       footer tick is the same point, held on), and prints as
                         EVENT turn seq=<n> report=<id> rows=<n> <summary>
                       ONE line per worker turn — report= the row id, rows=
                       the body's row count, the summary the last row said —
                       when the report is THIS turn's: one that came after
                       the worker was read busy for the turn (however long
                       ago: a report posted mid-turn) or after the point; one
                       from before the last point handed over, or between it
                       and the worker's next busy read (the ended turn's late
                       report), never is; one from before the turn was seen
                       to begin at all — the loop's first turn, a turn too
                       short to be read busy — is, when it came within
                       --report-window S (default 120) before the point, the
                       only thing then known about it. With none in the grace,
                         EVENT idle-no-report seq=<n> [complete=<0|1> rows=<n>] <summary>
                       (--report's brief rides only on that line: a folded
                       turn reads no report from the screen, the mail IS what
                       was said). Measured 2026-09-14: one wake and one 2 KB
                       `inbox get` per turn, where the same turn was a 689-row
                       `report` read. A question, a limit notice, a prompt
                       are not held: they print as they always did, between
                       the MAIL lines. --journal records the MAIL lines (kind
                       `mail`) and the fold (`report`, `rows`). The lane that
                       cannot go on — a host without the fabric verbs, no
                       $ATERM_PARENT_SESSION_ID for @self, its reconnect
                       window lapsed — says `MAIL lane off: <why> (the loop
                       goes on without mail)` once, and from then on the
                       lines are as without the flag; the budget bounds the
                       hold as it bounds every wait. supervise --mail says
                       the MAIL lines on stderr, holds its idle review point
                       the same way, and adds `report <id> rows=<n>` (or
                       `report -`) after the phase lines of its result; the
                       journal gets the `EVENT turn` line. --mail needs the
                       worker's @sid (only that worker's report is folded).
                       The lane's parked wait is cut short when the loop ends
                       (its aterm-ctl is signalled), so supervise --mail hands
                       its result back the moment the point is reached and
                       watch --mail's process ends with its last line; a
                       parked wait that outlives an aterm self-update lists
                       the successor's inbox whole and takes up from the bus
                       offset (the successor counts its rows from 1 again).
                       Without the flag, every line is byte for byte what it
                       was.
    --reconnect-s S    (await-turn, supervise, watch) An aterm self-update hands
                       every session to the new instance under the same @sid, and
                       a request in flight may get no answer (`server closed the
                       connection without responding`; a socket refused, gone,
                       reset or timed out) or be turned away unread (`ERR control
                       server busy; retry`, `ERR auth`). That OUTAGE is ridden
                       out, not the end: one line `RECONNECT <reason>` (cut at
                       160 characters), then a screen read of the same @sid,
                       retried 0.5 s apart doubling to 8 s, until one answers —
                       `RECONNECTED after <ms> ms` — and the loop looks again
                       from a fresh read. S (default 180; 0 = off, the failure
                       ends the loop) bounds the whole outage, from its first
                       unserved request until the loop gets past it (that kind
                       of request served again, the screen seen to move, or a
                       whole look done): a request dropped again after the
                       RECONNECTED is the same outage — nothing more is printed,
                       the retries keep backing off, the window keeps running.
                       While an outage lasts, `ERR no such session` is not yet
                       an answer (the new instance may not host the @sid yet);
                       outside one it ends the loop at once, and `ERR exited`
                       always does. After the outage no seq from before is
                       waited on (the content seq starts over on the new
                       instance), the point reported before it is reported once
                       more if it is still showing, and a press whose answer
                       never came is not repeated blind or counted as an
                       approval — the box is read and classified again first (on
                       a host without the guard, a `1` the server confirmed is
                       the approval it was, and a digit left in the composer is
                       backspaced after the reconnect). A wait that runs out is
                       followed by a read, so a handoff no request saw fail is
                       noticed too (the seq read is below the one waited on). An
                       outage ends the loop when its window lapses, with
                       `reconnect window lapsed: <the last failure>` (watch: an
                       `EXIT` line; await-turn and supervise: the error, exit 1)
                       — and when --max-s or --timeout runs out first, that is
                       the TIMEOUT, exit 124. watch prints its lines on stdout
                       (informational: nothing to do), await-turn and supervise
                       on stderr. Every request, the probe included, resolves
                       the socket afresh: with no --socket and no
                       $ATERM_CONTROL_SOCK, the instance hosting this terminal,
                       else the newest — after an update, the new instance, which
                       hosts the @sid or forwards to the one that does. A
                       per-instance socket named there (`aterm-<pid>.sock`, as
                       `aterm ctl instances` prints) goes with its instance, so
                       every ride-out through one lapses: leave both unset, or
                       name the `aterm.sock` alias.
    --dismiss-surveys  (supervise, watch) Dismiss Claude Code's session survey
                       rather than report it. When it appears, press `0` — only
                       `0`, never a rating — GUARDED: `key
                       if=^●.How.is.Claude.doing 0`, the check and the press
                       under one lock (`OK skipped` means no row matched and
                       nothing was written), then look again from a fresh read.
                       The survey gone there, print `DISMISSED survey seq=<n>`
                       (the press's seq; watch: stdout, supervise: stderr) and
                       append one --notes line — nothing for a skipped press.
                       Still open there (the `0` did not take, or the guard
                       matched no row of it), it is handed to you: the `EVENT
                       survey` line and a --notes line, and it is not pressed
                       again while it stays open. A `0` that landed in the
                       composer instead (the survey had left first, or did not
                       take it) is backspaced and noted, and nothing is
                       dismissed. Nothing is pressed while a prompt box is up or
                       text is typed in the composer (the survey waits), and a
                       host without `key if=` gets no `0` at all: the survey is
                       reported as without the flag.
    --context-warn PCT (supervise, watch) Watch Claude Code's context indicator
                       (`<n>% until auto-compact` above the composer; see phase) on
                       every read of a turn, a busy one included. The first reading
                       at or below PCT (default 10; 0 to 100; 0 = off: neither
                       line) prints `EVENT context seq=<n> <v>% until
                       auto-compact`, once a descent. After it, a read that shows
                       the composer frame, no approval box and no indicator — or
                       one that reads 30 points or more over the last — prints
                       `EVENT compacted seq=<n>` once, and the warning is armed
                       again for the next descent. watch: stdout; supervise:
                       stderr. supervise's watch lasts one run: each run starts
                       armed (one that starts low warns again), so a compaction
                       between two runs, while you act on a result, leaves it
                       nothing to see and prints no `EVENT compacted`. After an
                       `EVENT context`, check `phase` before the next run: no
                       `context <n>%` line (and no box up) is the compaction.
                       watch keeps one watch for as long as it runs. Only the
                       indicator is read: nothing Claude Code says about
                       compacting. A compaction replaces the worker's history
                       with a summary, and standing rules you gave it (run
                       nothing heavy while a flag file exists, say) can silently
                       drop out. On `EVENT context`, have the worker bring its
                       handoff and notes up to date before it compacts; on
                       `EVENT compacted`, re-send your standing rules in one
                       turn. Never type /compact or /clear into the worker for it
                       without the human.
    --journal FILE     (supervise, watch) Append one JSON object per line the
                       loop prints — and, for supervise, per line watch WOULD
                       have printed for what it decides silently: its
                       approvals, its review point, its TIMEOUT, or `EXIT
                       <reason>` for the error it ends on.
                         {\"t\":<unix ms>,\"sid\":\"<sid>\"|null,\"kind\":\"event|
                          approved|dismissed|reconnect|timeout|exit|mail|extend|
                          escalated|cleared|probe\",\"phase\":
                          \"idle|question|prompt|limited|survey|context|
                          compacted|turn|idle-no-report|resumed|still-limited|
                          rebriefed|rebrief-failed|-\",\"seq\":<n>|null,
                          \"complete\":0|1|null,
                          \"rows\":<n>|null,\"summary\":\"<the line's free-text
                          tail>\",\"line\":\"<the exact line>\",\"turn\":<id>|null}
                       Every field is read from the line itself, so the record
                       and the line cannot disagree; `turn` is the ledger turn
                       `--report` counted its report from. The file is opened
                       append-only and created 0600 when missing — one that is
                       already there keeps its mode and its lines — and a
                       failure to open or write it is said ONCE on stderr and
                       never stops the loop. `aterm drive ledger --journal
                       FILE` replays it. --notes is still what it was: one line
                       per Bash-prompt decision, and nothing else.
    --resume [RULES]   (watch) Live through a usage limit's reset and probe
                       the worker after it (see watch); RULES, when named, is
                       the file whose contents are restated to the worker as
                       ONE turn once it answers — keep your standing rules
                       there (run nothing heavy while a flag file exists, …),
                       and edit it as they change: it is read when sent.
                       Refused at the launch when it cannot be read or is
                       empty. The next word is the file unless it is a flag
                       or the worker's @sid.

OPTIONS
    --socket PATH   The target aterm's control socket. Defaults to
                    $ATERM_CONTROL_SOCK, else the instance hosting this
                    terminal, else the newest local instance.
    --dial NAME     Drive a REMOTE aterm: relay to the saved connection NAME via the
                    local host's `dial` verb, then run `prompt` there — byte-identical
                    to a local turn, with predicates evaluated on the remote host. The
                    local socket/token come from --socket / $ATERM_CONTROL_SOCK /
                    $ATERM_CONTROL_TOKEN. Example: aterm-drive --dial work prompt '...'
    --idle MS       Quiescence window that counts as 'turn complete' (default 600).
                    Bigger = more certain the turn ended; smaller = snappier.
    --timeout MS    Give up after this long (default 180000; the host caps any
                    single await at 600000, so larger values end there).
    --ready REGEX   The prompt-ready row pattern for the BEST-EFFORT settle confirm
                    after idle. Default matches a Claude input caret, which is only
                    right when the driven program IS Claude — point it at your own
                    REPL's prompt otherwise, or pass '' for idle-only. Also settable
                    as $ATERM_DRIVE_READY (the flag wins). A non-matching pattern
                    costs a bounded extra wait, never a failed turn.

WHICH `await` TO USE
    * A TUI with an animated spinner → `prompt` (idle works: the spinner keeps the
      screen changing until the turn ends).
    * Driving Claude Code / an agent whose screen sits STATIC for seconds mid-turn
      (its prompt glyph stays on screen while it thinks, so matching it returns
      mid-turn too) → `prompt` can settle early; the signal that holds is its busy
      footer LEAVING: `await gone esc.to.interrupt` right after the prompt,
      while the footer is up (gone is level-triggered: a footer that is already
      absent answers at once). A regex is ONE whitespace-free token — the wire
      joins argv with spaces and never quotes, so a quoted 'esc to interrupt'
      arrives as three words and arms `esc` alone.
    * A command that pauses SILENTLY mid-run (e.g. `sleep`) → don't trust idle
      alone; use `await match <regex>` on a known output marker instead.
    * A plain shell command → `await block` (waits for the command to finish).

EXAMPLES
    # one driven turn against Claude Code:
    aterm-drive prompt 'Refactor utils.rs to drop the unwrap() calls.'
    # wait for a specific marker rather than idle:
    aterm-drive await match BUILD.SUCCESSFUL
    # the turn is over when Claude Code's busy footer leaves the screen:
    aterm-drive await gone esc.to.interrupt
    # capture the terminal content as rendered pixels (the reply names the path):
    aterm-drive shot screen.png
    # is this line a read? (exit 0/1; the reason names the rule)
    aterm-drive classify 'git status --short && git pull | tail'
    # what is the worker doing right now?
    aterm-drive phase @s-1e918c46
    # block until its turn ends (or 10 min), then say what it needs:
    aterm-drive await-turn @s-1e918c46 --timeout 600000
    # keep it moving through its reads; stop at the first thing that needs you:
    aterm-drive supervise @s-1e918c46 --auto-reads --max-s 1800 --notes notes.txt
    # the same loop, never exiting at a review point: one stdout line per
    # decision — run it under your harness's background monitor:
    aterm-drive watch @s-1e918c46 --auto-reads --notes notes.txt --report
    # everything the worker said since your turn, what scrolled off included:
    aterm-drive report @s-1e918c46

GOTCHA
    Submit with a real Enter keypress (this tool uses `key enter`), never a raw
    newline byte — a TUI line editor reads Enter as a keypress (CR), not LF.";

#[cfg(test)]
mod tests {
    use super::*;

    /// A mock transport recording the driven verbs and returning a canned screen.
    struct MockClient {
        sent: Vec<u8>,
        entered: u32,
        screen: String,
    }
    impl ControlClient for MockClient {
        type Error = std::convert::Infallible;
        fn send(&mut self, bytes: &[u8]) -> Result<(), Self::Error> {
            self.sent.extend_from_slice(bytes);
            Ok(())
        }
        fn key_enter(&mut self) -> Result<(), Self::Error> {
            self.entered += 1;
            Ok(())
        }
        fn await_idle_and_ready(
            &mut self,
            _idle: Duration,
            _ready: &str,
            _timeout: Duration,
        ) -> Result<String, Self::Error> {
            Ok(self.screen.clone())
        }
    }

    #[test]
    fn turn_sends_prompt_presses_enter_and_returns_settled_screen() {
        let mut client = MockClient {
            sent: Vec::new(),
            entered: 0,
            screen: "⏺ ANSWER: 391\n❯ ".to_string(),
        };
        // A permissive governor (cross-session driving): enabled, ample tokens.
        let mut gov = SelfGovernor::disabled(8, 1, 1_000_000);
        gov.enable_self_write();
        let turn = Turn::default();
        let out = turn.run(&mut client, &mut gov, b"what is 17*23?").unwrap();
        assert_eq!(client.sent, b"what is 17*23?");
        assert_eq!(
            client.entered, 1,
            "submitted with exactly one Enter keypress"
        );
        assert!(out.contains("ANSWER: 391"));
    }

    #[test]
    fn governor_is_off_by_default_fail_closed() {
        // The default posture refuses self-writes entirely (R4 safety).
        let mut gov = SelfGovernor::disabled(8, 1, 1000);
        assert!(!gov.allow_self_write(), "self-write off by default");
        gov.enable_self_write();
        assert!(gov.allow_self_write(), "enabled + has tokens -> allowed");
    }

    #[test]
    fn governor_rate_limits_and_breaker_latches_fail_closed() {
        let mut gov = SelfGovernor::disabled(2, 1, 10);
        gov.enable_self_write();
        assert!(gov.allow_self_write()); // token 2 -> 1
        assert!(gov.allow_self_write()); // token 1 -> 0
        assert!(!gov.allow_self_write(), "bucket empty -> refused");
        gov.tick(); // refill 1
        assert!(gov.allow_self_write());
        // Sustained self-output trips the breaker; thereafter ALL writes refused.
        gov.note_self_output(100);
        assert!(gov.tripped());
        gov.tick(); // even with tokens, a tripped breaker refuses
        assert!(
            !gov.allow_self_write(),
            "tripped breaker is fail-closed regardless of tokens"
        );
        gov.reset();
        assert!(gov.allow_self_write(), "operator reset recovers");
    }

    #[test]
    fn tick_saturates_when_capacity_plus_refill_overflows_u32() {
        // capacity + refill > u32::MAX would overflow a plain add (debug panic /
        // release wrap). Every sibling op saturates; tick must too. The `.min`
        // still clamps, so tokens settles at capacity.
        let mut gov = SelfGovernor::disabled(3_000_000_000, 2_000_000_000, u32::MAX);
        gov.tick(); // must not panic (would overflow a non-saturating add)
        assert_eq!(
            gov.tokens, gov.capacity,
            "tick refills up to capacity without overflowing"
        );
    }

    #[test]
    fn turn_is_governed_when_self_write_disabled() {
        let mut client = MockClient {
            sent: Vec::new(),
            entered: 0,
            screen: String::new(),
        };
        let mut gov = SelfGovernor::disabled(8, 1, 1000); // NOT enabled
        let turn = Turn::default();
        assert!(matches!(
            turn.run(&mut client, &mut gov, b"x"),
            Err(TurnError::Governed)
        ));
        assert!(client.sent.is_empty(), "no bytes sent when governed");
    }

    #[test]
    fn run_rejects_a_bad_ready_pattern_before_touching_the_transport() {
        let mut client = MockClient {
            sent: Vec::new(),
            entered: 0,
            screen: String::new(),
        };
        let mut gov = SelfGovernor::disabled(8, 1, 1000);
        gov.enable_self_write();
        let turn = Turn {
            ready_pattern: "(unclosed".to_string(),
            ..Turn::default()
        };
        assert!(matches!(
            turn.run(&mut client, &mut gov, b"x"),
            Err(TurnError::BadPattern(_))
        ));
        assert!(client.sent.is_empty(), "no bytes sent on a bad pattern");
    }

    // --- RelayClient (the remote/persistent-connection ControlClient) ---

    /// An in-memory transport for `RelayClient`: records everything written, and
    /// hands back a canned sequence of server response bytes.
    struct RecordingTransport {
        sent: Vec<u8>,
        to_read: std::io::Cursor<Vec<u8>>,
    }
    impl RecordingTransport {
        fn new(canned: &[u8]) -> Self {
            Self {
                sent: Vec::new(),
                to_read: std::io::Cursor::new(canned.to_vec()),
            }
        }
    }
    impl Read for RecordingTransport {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            self.to_read.read(out)
        }
    }
    impl Write for RecordingTransport {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            self.sent.extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Canned server responses for one full driven turn, in order: send -> `OK`,
    /// key enter -> `OK`, await idle -> `OK idle 7`, await match -> `OK match 1`,
    /// text -> `OK 2` + two body lines (`⏺ ANSWER: 391`, `❯ `).
    const CANNED_TURN: &[u8] =
        b"OK\nOK\nOK idle 7\nOK match 1\nOK 2\n\xe2\x8f\xba ANSWER: 391\n\xe2\x9d\xaf \n";

    /// Tier-1 conformance: the bytes `RelayClient` emits for a driven `Turn` are
    /// byte-identical to the control lines `CtlClient` drives through `aterm-ctl`,
    /// and the parsed `text` payload matches `aterm-ctl`'s stdout — the property that
    /// makes "identical either way" (a `Turn` over a local vs a remote client) TRUE.
    #[test]
    fn remote_relay_client_wire_is_ctl_byte_identical() {
        let mut client = RelayClient::new(RecordingTransport::new(CANNED_TURN));
        let mut gov = SelfGovernor::disabled(8, 1, 1_000_000);
        gov.enable_self_write();
        let out = Turn::default()
            .run(&mut client, &mut gov, b"what is 17*23?")
            .expect("driven turn");

        // Exact wire bytes, verb-for-verb, in order — the same request lines
        // `CtlClient` frames from its argv (send/key/await idle/await match/text).
        let expected_wire = concat!(
            "send what is 17*23?\n",
            "key enter\n",
            "await idle 600 timeout 180000\n",
            "await match (^|\\s)\u{276f}(\\s|$) timeout 250\n",
            "text\n",
        );
        assert_eq!(
            String::from_utf8(client.io.sent.clone()).unwrap(),
            expected_wire,
            "RelayClient wire bytes must equal the control lines CtlClient drives"
        );
        // The settled screen equals the `text` body (each line + '\n'), byte-identical
        // to aterm-ctl's captured stdout.
        assert_eq!(out, "\u{23fa} ANSWER: 391\n\u{276f} \n");
    }

    /// Negative control (PROVES-and-CATCHES): the discarded `await match` reply MUST
    /// be consumed before `text`, or the persistent stream desyncs. A sequence that
    /// skips the consume reads the stale `OK match 1` line as the `text` header and
    /// fails — proving the consume is load-bearing.
    #[test]
    fn skipping_the_discarded_await_match_reply_desyncs_the_stream() {
        // Responses from `await idle` onward: OK idle 7, OK match 1, OK 2 + lines.
        let canned = b"OK idle 7\nOK match 1\nOK 2\nline-a\nline-b\n";
        let mut client = RelayClient::new(RecordingTransport::new(canned));
        client
            .request_status("await idle 600 timeout 180000")
            .unwrap();
        // BUGGY: write the await-match line but do NOT consume its reply.
        client.write_line("await match X timeout 250").unwrap();
        // `text` now reads the stale `OK match 1` as its header -> malformed count.
        let desynced = client.read_text();
        assert!(
            desynced.is_err(),
            "skipping the await-match consume desyncs: `text` reads a stale reply line"
        );
    }

    #[test]
    fn relay_client_rejects_newline_in_send_payload() {
        let mut client = RelayClient::new(RecordingTransport::new(b""));
        let err = client.send(b"first\nsend evil").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert!(
            client.io.sent.is_empty(),
            "nothing written on a rejected payload"
        );
    }

    #[test]
    fn relay_client_rejects_line_terminators_at_the_framing_boundary() {
        let mut client = RelayClient::new(RecordingTransport::new(b""));
        let error = client
            .write_line("AUTH token\nsend injected")
            .expect_err("a request must occupy exactly one line");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(client.io.sent.is_empty());
    }

    #[test]
    fn relay_client_maps_err_reply_to_error() {
        let mut client = RelayClient::new(RecordingTransport::new(b"ERR denied\n"));
        let err = client.send(b"hello").unwrap_err();
        assert_eq!(err.to_string(), "ERR denied");
    }
}
