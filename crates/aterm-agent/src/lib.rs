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

/// The Claude-prompt-ready signal: the bottom rows show the input box (`❯`) with
/// no in-flight spinner. These patterns are Claude-specific and live ONLY here.
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
                 driving ITSELF and the feedback floor tripped or self-writes are \
                 off. Fix: enable self-writes deliberately (SelfGovernor::\
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
/// Re-export so callers can match on a compile failure without depending on
/// `regex` directly (it is validated through `aterm-observe`).
/// The `aterm fleet` CLI (binary-era `aterm-fleet`), callable in-process.
pub mod fleet_cli;

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
}

impl CtlClient {
    /// Build a client. `ctl` is the path to `aterm-ctl`; `socket` is an explicit
    /// `--sock` path, or `None` to use `$ATERM_CONTROL_SOCK` / the default.
    pub fn new(ctl: impl Into<std::path::PathBuf>, socket: Option<String>) -> Self {
        Self {
            ctl: ctl.into(),
            socket,
        }
    }

    /// Run `aterm-ctl [--sock S] <args...>`, returning stdout or a trimmed stderr.
    pub fn run(&self, args: &[&str]) -> Result<String, String> {
        let mut cmd = std::process::Command::new(&self.ctl);
        if let Some(s) = &self.socket {
            cmd.arg("--sock").arg(s);
        }
        cmd.args(args);
        let out = cmd
            .output()
            .map_err(|e| format!("could not run {}: {e}", self.ctl.display()))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
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
    socket. This tool reads the live screen and drives keystrokes over that socket
    via `aterm-ctl` — the same engine a human types into. The key primitive is
    `await`: block until the surface reaches a condition, so you never sleep-and-
    hope or scrape for a spinner.

USAGE
    aterm-drive [--socket PATH] [--idle MS] [--timeout MS] [--ready REGEX]
                <command> [text...]

COMMANDS
    prompt <text...>   Type <text>, press Enter, then BLOCK until the agent's turn
                       settles (no screen change for --idle ms), and print the
                       settled screen. This is the one you want for a drive loop.
    read               Print the live screen (one row per line).
    await <cond>       Block until a condition, then print the kernel's verdict:
                         idle <ms>        surface unchanged for <ms> (turn done)
                         match <regex>    a visible row matches <regex>
                         seq              the next content change lands
                         block            a shell command completes (OSC-133)
    shot [path]        Save a pixel-true PNG of the terminal content view (the
                       rendered cells; OS chrome/titlebar are NOT captured).
    help               Show this text.

OPTIONS
    --socket PATH   The target aterm's control socket. Defaults to
                    $ATERM_CONTROL_SOCK, else the newest local instance.
    --dial NAME     Drive a REMOTE aterm: relay to the saved connection NAME via the
                    local host's `dial` verb, then run `prompt` there — byte-identical
                    to a local turn, with predicates evaluated on the remote host. The
                    local socket/token come from --socket / $ATERM_CONTROL_SOCK /
                    $ATERM_CONTROL_TOKEN. Example: aterm-drive --dial work prompt '...'
    --idle MS       Quiescence window that counts as 'turn complete' (default 600).
                    Bigger = more certain the turn ended; smaller = snappier.
    --timeout MS    Give up after this long (default 180000).
    --ready REGEX   The prompt-ready row pattern for the BEST-EFFORT settle confirm
                    after idle. Default matches a Claude input caret, which is only
                    right when the driven program IS Claude — point it at your own
                    REPL's prompt otherwise, or pass '' for idle-only. Also settable
                    as $ATERM_DRIVE_READY (the flag wins). A non-matching pattern
                    costs a bounded extra wait, never a failed turn.

WHICH `await` TO USE
    * Driving Claude / a TUI with an animated spinner → `prompt` (idle works: the
      spinner keeps the screen changing until the turn ends).
    * A command that pauses SILENTLY mid-run (e.g. `sleep`) → don't trust idle
      alone; use `await match <regex>` on a known output marker instead.
    * A plain shell command → `await block` (waits for the command to finish).

EXAMPLES
    # one driven turn against Claude Code:
    aterm-drive prompt 'Refactor utils.rs to drop the unwrap() calls.'
    # wait for a specific marker rather than idle:
    aterm-drive await match 'BUILD SUCCESSFUL'
    # capture the terminal content as rendered pixels:
    aterm-drive shot /tmp/screen.png

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
    fn relay_client_maps_err_reply_to_error() {
        let mut client = RelayClient::new(RecordingTransport::new(b"ERR denied\n"));
        let err = client.send(b"hello").unwrap_err();
        assert_eq!(err.to_string(), "ERR denied");
    }
}
