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
        let mut cmd = std::process::Command::new(&self.ctl);
        if let Some(s) = &self.socket {
            cmd.arg("--sock").arg(s);
        }
        cmd.args(args);
        let out = cmd
            .output()
            .map_err(|e| format!("could not run {}: {e}", self.ctl.display()))?;
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
                          [--reconnect-s S]
              | watch [@sid] [--auto-reads] [--allow-python GLOB]... [--notes FILE] [--max-s S]
                      [--reconnect-s S]

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
    await-turn [@sid] [--timeout MS] [--reconnect-s S]
                       Block until the phase is no longer busy, then print it like
                       `phase`. The loop is `await idle 2000` → read → `await seq`
                       (never a sleep); where the host knows `await gone`, the busy
                       footer LEAVING is the first wait. A screen without the
                       composer rules (a build, a script, a REPL) whose output
                       never held still for the 2 s is busy too: its turn ends
                       when the output pauses. Exit 124 on --timeout (default: the
                       global --timeout) with the worker still busy — or with the
                       connection lost (see --reconnect-s): then the phase of the
                       last screen read, or `busy` with `reason no screen: no
                       read answered before the timeout` when none was.
    supervise [@sid] [--auto-reads] [--max-s S] [--allow-python GLOB]... [--notes FILE]
              [--reconnect-s S]
                       The loop: await-turn; with --auto-reads, a Bash prompt whose
                       command classifies read-only is approved (option 1, pressed
                       GUARDED: `key if=Do.you.want.to.proceed 1` on a host that has
                       it — `OK seq=<n>` is the press, `OK skipped seq=<n>` means
                       NOTHING was pressed or approved: the box had left (the loop
                       goes on), or the seq is the screen just parsed and the
                       guard matched no row (that box is handed to you); a host
                       without the guard answers a usage line or a bare `ERR` and
                       the press falls back to read → confirm → press → re-read,
                       backspacing a digit that landed in the composer; `ERR busy
                       sink` is retried, any other `ERR` stops the loop), one line
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
                       lost connection is ridden out is the TIMEOUT too.
    watch [@sid] [--auto-reads] [--allow-python GLOB]... [--notes FILE] [--max-s S]
          [--reconnect-s S]
                       supervise's loop for a harness that wakes its agent once per
                       stdout line (a background monitor, a supervisor process).
                       Approvals are supervise's, and each prints `APPROVED
                       seq=<n> <command>`; --notes gets supervise's lines. A
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
                       on stderr).
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
    aterm-drive watch @s-1e918c46 --auto-reads --notes notes.txt

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
