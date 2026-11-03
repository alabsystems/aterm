// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#![cfg_attr(all(anchor_rules, any(test, debug_assertions)), feature(macro_attr))]

//! Agent interface layer ([`supervise`], [`harness`], [`drive_cli`], [`fleet_cli`], [`ai`]).
//! Control clients: [`CtlClient`] and [`RelayClient`].
#![cfg_attr(
    test,
    allow(
        clippy::disallowed_types,
        reason = "test code: a stand-in child may inherit descriptors"
    )
)]

#[cfg(test)]
aterm_durable::plain_durable_flushes_in_tests!();

#[cfg(test)]
mod test_program;

/// A test's POSIX absolute path literal as this host reads it absolute, as a `&'static str`
/// (`harness::host_path::abs` makes the same `String`): itself on Unix, on drive `C:` on Windows;
/// with a text before and after it, that text with the path inside.
#[cfg(test)]
macro_rules! at {
    ($posix:literal) => {
        at!("", $posix, "")
    };
    ($before:literal, $posix:literal, $after:literal) => {
        if cfg!(windows) {
            concat!($before, "C:", $posix, $after)
        } else {
            concat!($before, $posix, $after)
        }
    };
}

#[cfg(all(test, unix))]
aterm_secrets::memoized_x25519_in_tests!();

/// The crate's durability flush ([`durable::sync_all`]).
mod durable;

use std::io::{Read, Write};

/// AI connections: one working login per AI CLI, its doctor, and reviews across AIs.
pub mod ai;
/// The `aterm drive` CLI, callable in-process.
pub mod drive_cli;
/// Swarm census (`aterm fleet census`, `FleetOp::Census`, and `/monitor`'s single-steward read).
#[cfg(unix)]
pub mod fleet_census;
/// The `aterm fleet` CLI, callable in-process.
pub mod fleet_cli;
/// Fleet admission as this Mac's membership log says it (F17).
#[cfg(unix)]
pub mod fleet_log;
/// `aterm fleet init|join|peers|allow|publish|sync|census|leave`: this machine's membership in a
/// fleet.
#[cfg(unix)]
mod fleet_member;
/// The production signed-image service backend and immutable native proof.
#[cfg(unix)]
pub mod service_activation;
/// Bounded signed service repair over the native fleet Drive transport.
pub mod service_repair_rpc;
#[cfg(all(unix, any(test, debug_assertions)))]
pub use fleet_member::test_sync_from_vault_at;
/// `aterm members`: Macs joining ALab and the members that let them in.
#[cfg(unix)]
pub mod members_cli;
/// Bounded, value-free release preparation observations over authenticated control.
#[cfg(unix)]
pub mod operational;
/// `vault-join`: a Mac whose vault key changed proves itself to a member over the fleet.
#[cfg(unix)]
pub mod vault_join;
/// `aterm ctl survey --fleet | --host`: the fleet's peers; `aterm ctl --host <h> …`: another Mac
/// driven over it.
#[cfg(unix)]
pub use fleet_member::{
    FleetCheck, FleetSync, FleetSyncError, check_from_cache, drive_fleet, drive_fleet_proven,
    survey_fleet, sync_from_cache, try_sync_from_cache,
};
/// The harness core.
pub mod harness;
/// The `systems.alab.aterm-node` background job (`aterm node serve | status | on | off`, C20–C23).
#[cfg(unix)]
pub mod node_service;
/// The launcher's half of `aterm --headless --ready-fd <n>`.
#[cfg(unix)]
pub mod ready;
/// The supervisor: the loop the window's own host runs on every agent session, its policy,
/// command classifier, journal, ledger and tapes.
pub mod supervise;
/// Swarm survey facts (C16): host label, window liveness, per-row account health, SSH host-key
/// fingerprint and vault-server subscriber last-heard records.
pub mod survey_facts;

/// The verb the one binary answers a control request on: `aterm ctl …`.
pub const CTL_VERB: &str = "ctl";

/// This process's own executable ([`CtlClient::own_at`]).
/// # Errors
/// The executable could not be read.
pub fn own_exe() -> Result<std::path::PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| {
        format!("cannot find this program's own executable for its ctl client: {e}")
    })?;
    Ok(resolved_exe(exe))
}

/// `exe` resolved through its links ([`own_exe`]).
fn resolved_exe(exe: std::path::PathBuf) -> std::path::PathBuf {
    match std::fs::canonicalize(&exe) {
        Ok(path) => path,
        Err(_) if cfg!(target_os = "linux") => std::path::PathBuf::from("/proc/self/exe"),
        Err(_) => exe,
    }
}

/// A client that drives a target aterm by shelling out to a control client ([`CtlClient::own_at`]).
pub struct CtlClient {
    ctl: std::path::PathBuf,
    /// The arguments before `--sock` (the `ctl` verb, for [`Self::own_at`]).
    lead: Vec<String>,
    socket: Option<String>,
}

impl CtlClient {
    /// Build a client.
    pub fn new(ctl: impl Into<std::path::PathBuf>, socket: Option<String>) -> Self {
        Self {
            ctl: ctl.into(),
            lead: Vec::new(),
            socket,
        }
    }

    /// A client run as `<exe> ctl …` ([`own_exe`]).
    pub fn own_at(exe: impl Into<std::path::PathBuf>, socket: Option<String>) -> Self {
        Self {
            lead: vec![CTL_VERB.to_string()],
            ..Self::new(exe, socket)
        }
    }

    /// Run `<ctl> [--sock S] <args...>`: its stdout on exit 0, else [`failure_words`].
    pub fn run(&self, args: &[&str]) -> Result<String, String> {
        let reply = self.run_raw(args)?;
        if reply.code == 0 {
            Ok(reply.stdout)
        } else {
            Err(failure_words(&reply))
        }
    }

    /// Run `<ctl> [--sock S] <args...>` and return the exit code with both streams.
    pub fn run_raw(&self, args: &[&str]) -> Result<supervise::CtlReply, String> {
        let mut cmd = aterm_uds::spawnfd::command(&self.ctl);
        cmd.args(&self.lead);
        if let Some(s) = &self.socket {
            cmd.arg("--sock").arg(s);
        }
        cmd.args(args)
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(aterm_uds::spawnfd::Stdio::piped())
            .stderr(aterm_uds::spawnfd::Stdio::piped());
        let out = cmd
            .spawn()
            .and_then(|child| child.wait_with_output())
            .map_err(|e| format!("could not run {}: {e}", self.ctl.display()))?;
        Ok(supervise::CtlReply {
            code: out.status.code().unwrap_or(1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

fn failure_words(reply: &supervise::CtlReply) -> String {
    let err = reply.stderr.trim();
    if !err.is_empty() {
        return err.to_string();
    }
    match reply.stdout.trim() {
        "" => format!("aterm-ctl exited {}", reply.code),
        out => format!("aterm-ctl exited {}: {out}", reply.code),
    }
}

const MAX_TEXT_LINES: usize = 200_000;
const MAX_LINE_BYTES: usize = 1 << 20; // 1 MiB
const MAX_REPLY_BYTES: usize = 256 << 20; // 256 MiB

#[inline]
fn is_ok_reply(header: &str) -> bool {
    header == "OK" || header.starts_with("OK ")
}

pub(crate) fn reply_count(header: &str) -> std::io::Result<Option<usize>> {
    let Some(token) = header
        .strip_prefix("OK ")
        .and_then(|rest| rest.split_whitespace().next())
    else {
        return Ok(None);
    };
    if token
        .strip_prefix('+')
        .unwrap_or(token)
        .bytes()
        .all(|b| b.is_ascii_digit())
    {
        token.parse().map(Some).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "reply count exceeds the numeric bound",
            )
        })
    } else {
        Ok(None)
    }
}

#[inline]
pub(crate) fn parse_uint<T: std::str::FromStr>(s: &str) -> Option<T> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

#[inline]
pub(crate) fn parse_int<T: std::str::FromStr>(s: &str) -> Option<T> {
    let digits = s.strip_prefix('-').unwrap_or(s);
    (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

/// How long one exchange on a persistent control connection may wait for the server to get to
/// it: a hang guard, never a latency budget. On a loaded machine an idle step took up to 57 s
/// (`docs/measured/idle-look-under-load-2026-10-23.md`), a whole-step observation. The guard
/// bounds the wait for an unanswered exchange.
pub const DEFAULT_REPLY_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

/// The wait the request `words` asks of the server, by the first `timeout=<ms>` (or
/// `timeout <ms>`) it names: a typed text's words come after it.
pub(crate) fn named_timeout(words: &[&str]) -> Option<std::time::Duration> {
    let ms = words.iter().enumerate().find_map(|(i, w)| {
        if *w == "timeout" {
            words.get(i + 1).and_then(|n| parse_uint::<u64>(n))
        } else {
            w.strip_prefix("timeout=").and_then(parse_uint::<u64>)
        }
    });
    ms.map(std::time::Duration::from_millis)
}

/// How long the request `line` may wait for its reply on a connection bounded by `bound`
/// ([`RelayClient::bounded`]): the bound, and on top the wait the request names.
fn exchange_wait(line: &str, bound: std::time::Duration) -> std::time::Duration {
    let words: Vec<&str> = line.split_whitespace().collect();
    bound.saturating_add(named_timeout(&words).unwrap_or_default())
}

/// Whether `e` ended an exchange whose reply did not come within its wait
/// ([`RelayClient::bounded`]): the request may have reached the server, which may still act on it,
/// so its outcome is unknown, never the server's refusal.
#[must_use]
pub fn unanswered(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

/// Arms a transport's read and write timeouts for one exchange ([`RelayClient::bounded`]).
type Arm<S> = fn(&S, std::time::Duration) -> std::io::Result<()>;

fn monotonic_now<S>(_: &S) -> std::time::Instant {
    std::time::Instant::now()
}

#[derive(Clone, Copy)]
struct ExchangeBudget {
    started: std::time::Instant,
    wait: std::time::Duration,
}

/// One persistent, authenticated control connection, the in-process twin of [`CtlClient`].
pub struct RelayClient<S: Read + Write> {
    io: S,
    /// Sends only [`READ_VERBS`] ([`Self::reads_only`]).
    reads_only: bool,
    /// Bytes read past the last consumed line boundary (the next reply's prefix).
    buf: Vec<u8>,
    /// The `OK` replies to `help <verb>` this connection has had ([`Self::help_counted`]).
    help: Vec<(String, (String, String))>,
    /// Each exchange's bound and how it arms the transport ([`RelayClient::bounded`]).
    bound: Option<(Arm<S>, std::time::Duration)>,
    /// Hangs the transport up once a reply is left unread, so the server reads its caller gone
    /// ([`RelayClient::bounded`]).
    hang_up: fn(&S),
    /// The monotonic clock, injected with a controlled transport in deadline tests.
    clock: fn(&S) -> std::time::Instant,
    /// One budget for writes, flush, header and the entire counted or framed reply.
    exchange: Option<ExchangeBudget>,
    /// Whether the last exchange received any bytes, even before a malformed/partial reply.
    reply_started: bool,
    /// A reply was left unread: what is left of it would answer the next request, so every
    /// later one is refused unsent ([`Self::reply_unread`]).
    unread: bool,
}

/// The verbs a read-only connection may send ([`RelayClient::reads_only`]).
pub const READ_VERBS: &[&str] = &[
    "text",
    "screen",
    "line",
    "cell",
    "cursor",
    "modes",
    "blocks",
    "history",
    "status",
    "timeline",
    "metrics",
    "await",
    "who",
    "sessions",
    "help",
    "meta",
    "ls",
    "doc",
    "native-act",
];

/// Whether the control request `line` is one [`READ_VERBS`] names, in its read form.
fn is_read_request(line: &str) -> bool {
    let mut words = line.split_whitespace().skip_while(|w| w.starts_with('@'));
    match words.next() {
        Some("meta") => words.next().is_none(),
        Some("native-act") => words.next() == Some("info") && words.next().is_none(),
        Some("metrics") => {
            let mut command = words.filter(|w| !matches!(*w, "json" | "--json"));
            matches!(command.next(), None | Some("percentiles")) && command.next().is_none()
        }
        Some("doc") => matches!(words.next(), Some("ls" | "read" | "log")),
        Some(verb) => READ_VERBS.contains(&verb),
        None => false,
    }
}

/// How many verbs' help one [`RelayClient`] remembers.
const HELP_MEMO_MAX: usize = 16;

impl<S: Read + Write> RelayClient<S> {
    /// Wrap an already-connected, authenticated transport.
    pub const fn new(io: S) -> Self {
        Self {
            io,
            reads_only: false,
            buf: Vec::new(),
            help: Vec::new(),
            bound: None,
            hang_up: |_| {},
            clock: monotonic_now::<S>,
            exchange: None,
            reply_started: false,
            unread: false,
        }
    }

    /// This connection, able to send the read verbs alone ([`READ_VERBS`]): every other request
    /// is refused before a byte is written (`PermissionDenied`).
    #[inline]
    #[must_use]
    pub const fn reads_only(mut self) -> Self {
        self.reads_only = true;
        self
    }

    /// Whether this connection may send `line` ([`Self::reads_only`]).
    #[inline]
    fn may_send(&self, line: &str) -> std::io::Result<()> {
        if !self.reads_only || is_read_request(line) {
            return Ok(());
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("a read-only control connection sends no `{line}`"),
        ))
    }

    /// `help <verb>` as [`Self::request_counted`] reads it, asked once per connection. Only an
    /// `OK` reply is kept, for up to `HELP_MEMO_MAX` verbs; anything else is asked again.
    /// # Errors
    /// As [`Self::request_counted`].
    pub fn help_counted(&mut self, verb: &str) -> std::io::Result<(String, String)> {
        if let Some((_, reply)) = self.help.iter().find(|(v, _)| v == verb) {
            return Ok(reply.clone());
        }
        let reply = self.request_counted(&format!("help {verb}"))?;
        if is_ok_reply(&reply.0) && self.help.len() < HELP_MEMO_MAX {
            self.help.push((verb.to_string(), reply.clone()));
        }
        Ok(reply)
    }

    fn read_reply(&mut self) -> std::io::Result<String> {
        let line = self.read_line()?;
        match aterm_types::control_verbs::host_gone(&line) {
            Some((pid, sock)) => Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                format!(
                    "control connection closed: the aterm (pid {pid}) hosting the session went \
                     away under the forward to {sock}"
                ),
            )),
            None => Ok(line),
        }
    }

    fn read_line(&mut self) -> std::io::Result<String> {
        let mut scanned = 0;
        loop {
            let end = self.buf[scanned..]
                .iter()
                .position(|&b| b == b'\n')
                .map(|pos| scanned + pos);
            if end.unwrap_or(self.buf.len()) > MAX_LINE_BYTES {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "control line exceeds the length bound",
                ));
            }
            if let Some(pos) = end {
                let mut line: Vec<u8> = self.buf.drain(..=pos).collect();
                line.pop(); // drop '\n'
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return String::from_utf8(line).map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, "non-utf8 control line")
                });
            }
            // This prefix has no newline. Search only appended bytes after the read;
            // a long, fragmented reply must not repeatedly scan its whole prefix.
            scanned = self.buf.len();
            let mut tmp = [0u8; 8192];
            let n = self.read_some(&mut tmp)?;
            if n == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "control connection closed",
                ));
            }
            self.buf.extend_from_slice(&tmp[..n]);
        }
    }

    /// Whether `line` may go out as one request ([`Self::may_send`], on one line). A refusal
    /// writes nothing.
    fn sendable(&self, line: &str) -> std::io::Result<()> {
        self.may_send(line)?;
        if line.contains(['\n', '\r']) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "control request must not contain an embedded line terminator",
            ));
        }
        Ok(())
    }

    fn write_line(&mut self, line: &str) -> std::io::Result<()> {
        self.sendable(line)?;
        self.write_all(line.as_bytes())?;
        self.write_all(b"\n")?;
        self.flush()
    }

    fn remaining(&self) -> std::io::Result<Option<std::time::Duration>> {
        let Some(budget) = self.exchange else {
            return Ok(None);
        };
        let elapsed = (self.clock)(&self.io).saturating_duration_since(budget.started);
        let left = budget.wait.saturating_sub(elapsed);
        if left.is_zero() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "control exchange exceeded its reply budget",
            ));
        }
        Ok(Some(left))
    }

    fn before_io(&self) -> std::io::Result<()> {
        if let Some(left) = self.remaining()?
            && let Some((arm, _)) = self.bound
        {
            arm(&self.io, left)?;
        }
        Ok(())
    }

    fn read_some(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        self.before_io()?;
        let got = self.io.read(bytes);
        self.reply_started |= got.as_ref().is_ok_and(|n| *n > 0);
        self.remaining()?;
        got
    }

    fn write_all(&mut self, mut bytes: &[u8]) -> std::io::Result<()> {
        while !bytes.is_empty() {
            self.before_io()?;
            let got = self.io.write(bytes);
            self.remaining()?;
            match got {
                Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
                Ok(n) => bytes = &bytes[n..],
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.before_io()?;
        let got = self.io.flush();
        self.remaining()?;
        got
    }

    /// Readies the connection for the request `line`: refused, unsent (`NotConnected`), once an
    /// earlier reply was left unread ([`Self::settled`]), else armed with its wait
    /// ([`Self::bounded`]).
    fn ready_for(&mut self, line: &str) -> std::io::Result<()> {
        if self.unread {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "an earlier reply on this control connection was left unread, and what is left \
                 of it would answer this request: not sent",
            ));
        }
        self.reply_started = !self.buf.is_empty();
        if let Some((arm, bound)) = self.bound {
            let wait = exchange_wait(line, bound).max(std::time::Duration::from_millis(1));
            let started = (self.clock)(&self.io);
            arm(&self.io, wait)?;
            self.exchange = Some(ExchangeBudget { started, wait });
        }
        Ok(())
    }

    /// `got`; an error leaves the connection refusing every later request, since what is left of
    /// the reply would answer it, and a [`Self::bounded`] one is hung up.
    fn settled<T>(&mut self, got: std::io::Result<T>) -> std::io::Result<T> {
        let got = got.and_then(|value| self.remaining().map(|_| value));
        // Streaming reads are not part of the completed request's total budget. Restore the
        // full per-I/O interval the connection had when it began, not a nearly spent remainder.
        let restore = match (self.exchange.take(), self.bound) {
            (Some(budget), Some((arm, _))) => arm(&self.io, budget.wait),
            _ => Ok(()),
        };
        let got = got.and_then(|value| restore.map(|()| value));
        if got.is_err() && !std::mem::replace(&mut self.unread, true) {
            (self.hang_up)(&self.io);
        }
        got
    }

    /// Whether a reply on this connection was left unread (a [`Self::bounded`] exchange past its
    /// wait, or a reply cut short): every later request is refused unsent, so a caller reads what
    /// it asks after as the socket's silence, never the server's answer.
    pub(crate) const fn reply_unread(&self) -> bool {
        self.unread
    }

    pub(crate) fn closed_before_reply(&self) -> bool {
        !self.reply_started
    }

    /// Observe one reply byte after the caller proves closure or empty EOF. Whether
    /// no reply byte preceded closure; any observed or buffered byte refuses recovery.
    fn closed_peer_without_reply(&mut self) -> bool {
        if !self.closed_before_reply() || !self.buf.is_empty() {
            return false;
        }
        match self.read_some(&mut [0u8; 1]) {
            Ok(0) => true,
            Ok(_) => false,
            Err(e) => {
                matches!(
                    e.kind(),
                    std::io::ErrorKind::UnexpectedEof
                        | std::io::ErrorKind::BrokenPipe
                        | std::io::ErrorKind::ConnectionReset
                ) || (e.kind() == std::io::ErrorKind::NotConnected && e.raw_os_error().is_some())
            }
        }
    }

    fn request_with<T>(
        &mut self,
        line: &str,
        read: impl FnOnce(&mut Self, String) -> std::io::Result<T>,
    ) -> std::io::Result<T> {
        self.sendable(line)?;
        self.ready_for(line)?;
        let got = self
            .write_line(line)
            .and_then(|()| self.read_reply())
            .and_then(|header| read(self, header));
        self.settled(got)
    }

    /// The underlying transport.
    pub const fn transport(&self) -> &S {
        &self.io
    }

    /// Send one control line and read one reply line (`OK …` or `ERR …`), returned verbatim.
    /// # Errors
    /// I/O errors writing the request or reading the reply (a reply past its wait on a
    /// [`Self::bounded`] connection), an embedded line terminator, or a connection an earlier
    /// reply was left unread on.
    pub fn request_line(&mut self, line: &str) -> std::io::Result<String> {
        self.request_with(line, |_, header| Ok(header))
    }

    /// Send one control line and read a counted reply.
    /// # Errors
    /// As [`Self::request_line`], or a count/body past the client's own reply bounds.
    pub fn request_counted(&mut self, line: &str) -> std::io::Result<(String, String)> {
        self.request_counted_with_limit(line, MAX_REPLY_BYTES)
    }

    /// Send one control line and read a counted reply with a smaller aggregate bound.
    /// The client's ordinary line and aggregate bounds still apply.
    /// # Errors
    /// As [`Self::request_counted`], or the body exceeds `max_bytes` including newlines.
    pub fn request_counted_with_limit(
        &mut self,
        line: &str,
        max_bytes: usize,
    ) -> std::io::Result<(String, String)> {
        self.request_with(line, |client, header| {
            let body = match reply_count(&header)? {
                Some(count) => client.read_body_lines(count, max_bytes.min(MAX_REPLY_BYTES))?,
                None => String::new(),
            };
            Ok((header, body))
        })
    }

    fn read_body_lines(&mut self, count: usize, max_bytes: usize) -> std::io::Result<String> {
        if count > MAX_TEXT_LINES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "reply line count exceeds the bound",
            ));
        }
        let mut body = String::new();
        for _ in 0..count {
            let line = self.read_line()?;
            if line.len() >= max_bytes.saturating_sub(body.len()) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "reply body exceeds the aggregate length bound",
                ));
            }
            body.push_str(&line);
            body.push('\n');
        }
        Ok(body)
    }

    /// Send one control line and read a byte-framed reply.
    /// # Errors
    /// As [`Self::request_line`], a body past the line bound, or a stream that ends inside the
    /// body.
    pub fn request_bytes(&mut self, line: &str) -> std::io::Result<(String, Vec<u8>)> {
        self.request_with(line, |client, header| {
            let body = client.read_body_bytes(reply_count(&header)?)?;
            Ok((header, body))
        })
    }

    fn read_body_bytes(&mut self, count: Option<usize>) -> std::io::Result<Vec<u8>> {
        let Some(count) = count else {
            return Ok(Vec::new());
        };
        if count > MAX_LINE_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "reply body exceeds the bound",
            ));
        }
        while self.buf.len() < count {
            let mut tmp = [0u8; 8192];
            let n = self.read_some(&mut tmp)?;
            if n == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "control connection closed inside a reply body",
                ));
            }
            self.buf.extend_from_slice(&tmp[..n]);
        }
        Ok(self.buf.drain(..count).collect())
    }

    /// Send one length-framed request.
    /// # Errors
    /// As [`Self::request_line`], and a `head` carrying a line terminator.
    pub fn request_frame(&mut self, head: &str, payload: &[u8]) -> std::io::Result<String> {
        // A framed request is a write (`feed-bin`, `paste-bin`): never read-only's.
        if self.reads_only {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("a read-only control connection sends no `{head}` frame"),
            ));
        }
        if head.contains(['\n', '\r']) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "control request must not contain an embedded line terminator",
            ));
        }
        self.ready_for(head)?;
        let mut frame = format!("{head} {}\n", payload.len()).into_bytes();
        frame.extend_from_slice(payload);
        let got = self
            .write_all(&frame)
            .and_then(|()| self.flush())
            .and_then(|()| self.read_reply());
        self.settled(got)
    }

    /// Read one line a server pushed on a connection that is already parked on a stream.
    /// Stream reads retain their transport's per-I/O timeout, outside request total budgets.
    /// # Errors
    /// I/O errors, and `UnexpectedEof` when the stream ends.
    pub fn next_pushed(&mut self) -> std::io::Result<String> {
        self.read_line()
    }
}

impl RelayClient<aterm_uds::CtlStream> {
    /// Connect to a local control socket and authenticate.
    /// # Errors
    /// I/O errors connecting to `sock_path` or writing the auth line.
    pub fn connect_local(sock_path: &str, token: &str) -> std::io::Result<Self> {
        let stream = aterm_uds::kept_out(|| aterm_uds::CtlStream::connect(sock_path))?;
        Self::authenticated(stream, token)
    }

    /// [`connect_local`](RelayClient::connect_local) with [`aterm_ctl::connect_then_read_token`].
    /// # Errors
    /// I/O errors connecting to `sock_path`, writing the auth line, or reading the token file.
    pub fn connect_local_beside(sock_path: &str) -> std::io::Result<Self> {
        let (stream, token) = aterm_ctl::connect_then_read_token(sock_path)?;
        let token = token.map_err(|e| std::io::Error::new(e.kind(), format!("token: {e}")))?;
        Self::authenticated(stream, &token)
    }

    fn authenticated(stream: aterm_uds::CtlStream, token: &str) -> std::io::Result<Self> {
        let mut client = Self::new(stream);
        client.write_line(&format!("AUTH {token}"))?;
        Ok(client)
    }

    /// Bound each validated request's writes, flush, header and entire reply body by one
    /// monotonic budget: `bound` plus its named server wait ([`named_timeout`]). Remaining
    /// time is armed before each blocking I/O and checked again afterward. A late reply is
    /// an error ([`unanswered`]): the socket is hung up, so a request the server has yet to act
    /// on is its departed caller's, and every later request is refused unsent
    /// ([`Self::reply_unread`]). A zero budget uses one millisecond.
    /// Connection setup is separate; [`Self::next_pushed`] retains stream timeout semantics.
    #[must_use]
    pub fn bounded(mut self, bound: std::time::Duration) -> Self {
        self.bound_by(bound);
        self
    }

    /// [`Self::bounded`] from the next request on.
    pub fn bound_by(&mut self, bound: std::time::Duration) {
        self.bound = Some((arm_ctl, bound));
        self.hang_up = |stream| {
            aterm_uds::hangup::hang_up(stream);
        };
    }
}

/// Arm one blocking operation with the exchange's remaining time. Socket timer granularity
/// can round it up; the monotonic clock is checked again before accepting the result.
pub(crate) fn arm_ctl(
    stream: &aterm_uds::CtlStream,
    wait: std::time::Duration,
) -> std::io::Result<()> {
    let wait = wait.max(std::time::Duration::from_millis(1));
    let armed = stream
        .set_read_timeout(Some(wait))
        .and_then(|()| stream.set_write_timeout(Some(wait)));
    #[cfg(target_os = "macos")]
    {
        accept_closed_arm(stream, armed)
    }
    #[cfg(not(target_os = "macos"))]
    {
        armed
    }
}

#[cfg(target_os = "macos")]
fn accept_closed_arm(
    stream: &aterm_uds::CtlStream,
    armed: std::io::Result<()>,
) -> std::io::Result<()> {
    match armed {
        // Darwin can reject timeout updates after full peer closure. That peer cannot
        // block another operation; let actual I/O expose EOF, queued bytes or its error.
        Err(e)
            if matches!(e.raw_os_error(), Some(libc::EINVAL) | Some(libc::ENOTCONN))
                && aterm_uds::hangup::peer_closed(stream) =>
        {
            Ok(())
        }
        got => got,
    }
}

/// The help for the `aterm drive` tool.
pub const DRIVE_HELP: &str = "\
aterm drive — the window harness's readers, run by hand.

MENTAL MODEL
    An aterm window supervises every Claude Code, agy and Muse Code session by
    default (`status supervisor=aterm-harness@<pid>`), under `[harness]`
    in `aterm.toml` (`aterm help harness`). It types only into an agent the kernel
    confirms by its running image (`status supervisor_act=on`); a program merely
    named like one (a link, a copy, `exec -a claude`, an npm `node … cli.js`
    install) is observed (`supervisor_act=observe`). `aterm drive` runs that supervisor's
    readers and tape tools from a terminal. Vendor `codex` reports through
    `status program_state=` and has no reader here; `codex-alab` supervises itself.
    Driving a session is `aterm ctl` (`aterm ctl help`): `aterm ctl @self spawn
    agent=claude -` starts a worker, `aterm ctl await task @<sid>,… since=0` waits
    for it, `aterm ctl @<sid> result` reads its report; `turn`, `text`, `image` and
    `await agent idle,prompt,question,wall` drive and watch a Claude Code, agy or
    Muse Code session; for a codex (whose `agent=` stays `unknown`),
    `await log kind=program since=<log_seq> ep=<log_ep> base=<log_base>`, all three read
    from `status` before typing to it, waits on its own report.
    `aterm ctl @sid meta set questions ask|recommended|long-term|short-term` sets how
    the window answers that session's questions (`ask` leaves them to a person or
    `aterm drive answer`); `meta unset questions` hands them back to `[harness]`
    within 2 s.

USAGE
    aterm drive [--socket PATH] classify | phase | answer | ledger | tape | replay | help
    `aterm drive`, `aterm drive help` and `aterm drive <command> --help` print this page.

COMMANDS
    classify [--allow-python GLOB]... <cmd...>
                       Judge a shell command line by the supervisor's read-only rules:
                       prints `read-only` (exit 0) or `not-read-only <reason>` (exit 1).
                       Flags come first; `--` ends them. Every segment's head, seen
                       through `env` and `xargs`, must be a known read-only program
                       (bare, or by path under /bin, /usr/bin, /sbin or /usr/sbin) or a
                       script run by `python`, `python2` or `python3` whose path, free of
                       `..` and `$`, matches an `--allow-python GLOB` (none by default).
                       Refuses file redirects, forms that write or run (`git pull`,
                       `git -c`, `sed -i`, `find -delete`, `python3 -c`, `sort -o`) and
                       assignments that change what runs (`PATH`, `HOME`, `GIT_*`,
                       `LD_*`/`DYLD_*`, pager, editor). `aterm ctl` and `aterm drive` are
                       judged by verb: ctl's reads (`status`, `text`, `await`, `result`,
                       `survey`, …), bare `meta` and `task`, and drive's `phase`,
                       `classify` and `ledger` without `--out` read; `meta set`,
                       `task mark`, `spawn` and `answer` write.
                       Then refuses what the classifier vets only against a list, as the
                       harness's rules and a box's `classify` word do: any assignment,
                       `env`, `export` or `xargs` itself, a `for` or `select` loop, an
                       unquoted glob in the words of a program off the glob-safe list
                       (`sort *.txt`; `ls *.txt | sort` is a read), and blanks the
                       screen may draw otherwise than the shell reads them (a
                       here-document; a backslash before a tab, a newline or the
                       line's end, or before a blank that another blank follows or
                       that ends the line).
                       Then refuses a read of a path the default secret list names
                       (`~/.ssh`, `~/.aws`, `.env`, `*.pem`, `id_rsa*`, `credentials*`,
                       `~/.config/gh`, `~/Library`, …, under this shell's HOME), but not
                       a pattern, a filter or a file `ls`/`wc` only lists or measures:
                       `not-read-only a read of a secret (<rule>) …`. A relative path
                       is read against the line's own `cd` targets (the verb knows no
                       session's cwd), a glob as any name it may expand to, and
                       `grep -r`, `rg` or `find -exec` by the trees they descend; a
                       path built at run time (`\"$VAR\"`, `$(…)`) is refused.
    phase [@sid] [--allow-python GLOB]...
                       Read @sid (default: the active tab) once with its program's reader
                       (`status program=`, else the screen) and print its phase,
                         busy | prompt | limited | idle | question
                       then that phase's details:
                         prompt:   `kind`, `command`, `description`, `note <text>`,
                                   `classify` (a Bash box), `option N <label>`,
                                   `cancel esc` | `exit esc` | `interrupt esc`, and
                                   `box <token>` on a question dialog (for `answer --box`)
                         busy:     `reason <where>: <rule>` (`its reader reads it busy`
                                   when agy or Muse Code name none)
                         limited:  `message <text>` and `reset <text|->`
                       `question` means the last thing said ends in `?`. Then come
                       `survey 0` when a rating survey that `0` dismisses is open (`survey
                       withheld (<why>)` when it must not be pressed) and `context <n>%`
                       when the screen shows the context left before auto-compact. A Codex
                       tab always reads `idle`.
    answer @sid [--box TOKEN] <answer...>
                       Choose in @sid's open question dialog by its program's grammar:
                         Claude Code: an option number or label; comma-separated
                           options on a multi-select; `submit` on the review tab. Moves
                           the focus to each and presses Enter, fenced on that row (never
                           a digit, Esc, typed text or the chat row).
                         agy: an option number or label; types the option's
                           digit, fenced on the title row.
                         Muse Code: an option number or label; moves the focus and
                           presses Enter, fenced on the question row.
                       A label matches whole, in any case. Each also takes `recommended`
                       (the window's own choice) and `human` (types nothing). Refused: a
                       free-text row (Muse's `None of the above` too) and a policy word
                       that names no option (`long-term`, `short-term`;
                       `meta set questions <word>` sets one). `--box TOKEN` (`phase`'s
                       `box`) refuses when the dialog on screen is another box. A
                       person's input within `[harness] human_grace_s` leaves the dialog
                       to them. Prints one line:
                         `ANSWERED @sid enters=<k> answer=<what> next=<next>` (exit 0;
                           <next> is the next question's text, `question` when that is
                           unread, `review` on the review tab, or `none`)
                         `LEFT @sid <why>` (a person's input, or `human`; exit 0)
                         `NO-BOX @sid <why>` (exit 1)
                         `REFUSED @sid <why>` (exit 2, nothing typed)
                         `NOT-SERVED @sid <why> (enters=<k>)` (exit 3)
    ledger [@sid] [--journal FILE] [--since TIME] [--format text|md|html] [--out PATH]
                       @sid's run on one time axis, joined from its `history` (turns),
                       reply sizes (`offscreen`), the window's journal (`--journal FILE`:
                       `<state>/drive/<sid>.journal.jsonl`) and fabric mail (`@self`'s
                       `inbox --peek --meta`). Prints SOURCES, SUMMARY and
                       a TIMELINE in `manager`, `worker`, `watcher` and `fabric` lanes as
                       text columns (default), Markdown tables (`md`) or one swimlane
                       HTML page (`html`). `--out PATH` writes it there (a new file 0600;
                       an existing one keeps its mode) and prints `wrote PATH`;
                       `open_ledger` (⇧⌘L, Ctrl+Shift+L) opens the focused tab's HTML page.
                       `--since TIME` keeps what happened from TIME on: Unix ms or
                       `YYYY-MM-DD[THH:MM[:SS]][Z|±HH:MM]`.
                       The session log's own records place turns and mail: a turn by its
                       record's time, joined by `ledger=`, a message by its `mail` mark,
                       joined by `inbox=`; one the log no longer holds has no time, and is
                       counted. `--source log` is accepted and changes nothing; `--source
                       anchor` (the clock anchor) was removed on 2026-10-23.
                       No @sid: the one session `--journal` names.
    tape cat FILE [--text] [--salvage] | tape show FILE --decisions [--salvage]
    tape stats FILE [--salvage]
                       Read a recorded `.atape` (binary, or `atape-text 1`) offline.
                       `cat` lists its items, each effect as `#N` (`door=N`'s N), or with
                       `--text` the round-trippable text form; `show --decisions` prints
                       each decision's screen, rule, outcome, act, reply and journal line;
                       `stats` prints its bytes, span, kB/h, item, effect and decision
                       counts. A damaged tape exits 2 unless `--salvage` keeps the whole
                       frames before the damage; a torn tail is read up to the tear and
                       noted on stderr.
    tape ls [@sid] | tape forget FILE|@sid...
                       The hosted tapes (macOS and Linux) in `<state>/drive/tapes/<sid>/`,
                       recorded when a dev build runs with `ATERM_HARNESS_TAPE` set and
                       swept past 256 MiB or 72 h (`tapes/kept/` and tapes being written
                       stay). `ls` lists them newest first (with @sid: its tapes and
                       `kept/`). `forget` deletes each FILE given as `ls` prints its path
                       (the one way to delete a kept tape) or every unkept tape of an
                       @sid, refusing before deleting anything when a FILE is no tape
                       there or an @sid has none.
    replay FILE [--until door=N]
                       Replay FILE's loop offline against its recorded observations,
                       comparing every request and output. Prints `REPRODUCED`, `ENDED`,
                       `REACHED` (`--until door=N`), `DIVERGED at #N` (`decision`,
                       `output` or `drift`) or `PANICKED`, then request counts, acts
                       (`> …`) and lines (`| …`).
    replay FILE --set k=v... | --policy FILE [--set k=v...] [--until door=N]
                       Counterfactual, in acts mode: replay under a candidate `[harness]`
                       policy (`--policy FILE`, `--set k=v` overrides; unknown keys and
                       bad values are refused), comparing only acts, up to the first
                       divergence. Adds `STALE`: the candidate asked a read the tape
                       never answered.
                       Both forms exit 0 (REPRODUCED, ENDED, REACHED), 1 (DIVERGED),
                       3 (STALE), 4 (PANICKED) or 2 (unreplayable tape, usage error).
    replay FILE... --shadow [--set k=v...] [--policy FILE]
                       Decide every recorded decision again under this build and the
                       candidate (else the recorded) policy; `needs:` names missing
                       inputs and `TAINT` marks rows after the first difference. On a
                       tape longer than the shadow ledger holds, `past the ledger:`
                       counts the rest and names the limit; none is decided again. Exit 0
                       (all matched), 1 (diverged), 2 (unshadowable tape, usage error)
                       or 3 (undecidable point, or one past the ledger).
    replay --check FILE|DIR... [--set k=v...] [--policy FILE]
                       Replay every tape named, and every `*.atape`/`*.atape.txt` under
                       each DIR, in acts mode: a verdict per tape and a count of each.
                       Exit 1 (any diverged), else 2 (one not replayed, no tapes, usage
                       error), else 3 (stale), else 4 (panicked), else 0.
    replay FILE --why --at seq=N|door=N|[YYYY-MM-DDT]HH:MM[:SS]|\"<journal line>\"
                       [--format text|md|html]
                       Explain the decision at a screen `seq=N`, an effect `door=N`, a
                       local time or a journal line: its screen, parsed box, inputs with
                       their source records, rule trace, act, reply, journal line, ledger
                       row and `needs:` gaps. Exit 0, or 2 when no decision matches or
                       FILE cannot be read.
    help               Show this text.

OPTIONS
    --socket PATH   Target aterm control socket (default: the instance hosting this
                    shell, else the newest local instance).

ERRORS
    Bad usage, an unknown command, or (for `phase` and `answer`) no reachable aterm
    prints `aterm drive: <why>` on stderr and exits 1, like `not-read-only` and
    `NO-BOX` (under `tape` and `replay`, 2). `phase` and `ledger` otherwise exit 0;
    `ledger` names a source it could not read under SOURCES.

EXAMPLES
    aterm drive classify 'git status --short && git pull | tail'
    aterm drive phase @s-1e918c46
    aterm ctl @s-1e918c46 meta set questions ask
    aterm drive answer @s-1e918c46 'Use the existing schema'
    aterm drive ledger @s-1e918c46 --format html --out /tmp/ledger.html";

#[cfg(test)]
mod tests {
    struct FragmentTransport {
        base: std::time::Instant,
        now: std::cell::Cell<std::time::Instant>,
        timeout: std::cell::Cell<std::time::Duration>,
        step: std::time::Duration,
        write_step: std::time::Duration,
        flush_step: std::time::Duration,
        write_chunk: usize,
        round_up: bool,
        fragments: std::collections::VecDeque<Vec<u8>>,
        sent: Vec<u8>,
    }

    impl FragmentTransport {
        fn new(fragments: &[&[u8]], step: std::time::Duration) -> Self {
            let base = std::time::Instant::now();
            Self {
                base,
                now: std::cell::Cell::new(base),
                timeout: std::cell::Cell::new(std::time::Duration::MAX),
                step,
                write_step: std::time::Duration::ZERO,
                flush_step: std::time::Duration::ZERO,
                write_chunk: usize::MAX,
                round_up: false,
                fragments: fragments.iter().map(|bytes| bytes.to_vec()).collect(),
                sent: Vec::new(),
            }
        }

        fn advance(&self, step: std::time::Duration) -> std::io::Result<()> {
            let allowed = self.timeout.get();
            let spent = if self.round_up {
                step
            } else {
                step.min(allowed)
            };
            self.now.set(self.now.get().checked_add(spent).unwrap());
            if step > allowed && !self.round_up {
                return Err(std::io::ErrorKind::TimedOut.into());
            }
            Ok(())
        }
    }

    impl std::io::Read for FragmentTransport {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.fragments.is_empty() {
                return Ok(0);
            }
            self.advance(self.step)?;
            let part = self.fragments.front_mut().unwrap();
            let count = part.len().min(buf.len());
            buf[..count].copy_from_slice(&part[..count]);
            part.drain(..count);
            if part.is_empty() {
                self.fragments.pop_front();
            }
            Ok(count)
        }
    }

    impl std::io::Write for FragmentTransport {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.advance(self.write_step)?;
            let count = bytes.len().min(self.write_chunk);
            self.sent.extend_from_slice(&bytes[..count]);
            Ok(count)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.advance(self.flush_step)
        }
    }

    fn arm_fragments(io: &FragmentTransport, wait: std::time::Duration) -> std::io::Result<()> {
        io.timeout.set(wait);
        Ok(())
    }

    fn fragment_clock(io: &FragmentTransport) -> std::time::Instant {
        io.now.get()
    }

    /// The injected clock advances only when a fragment's simulated blocking read completes.
    /// Four 3ms fragments each satisfy a 10ms per-I/O timeout, but exceed one exchange budget.
    #[test]
    fn a_fragmented_counted_reply_keeps_one_exchange_budget() {
        let wait = std::time::Duration::from_millis(10);
        let io = FragmentTransport::new(
            &[b"OK 1\n", b"r", b"o", b"w\n"],
            std::time::Duration::from_millis(3),
        );
        let mut client = RelayClient::new(io);
        client.bound = Some((arm_fragments, wait));
        client.clock = fragment_clock;
        let got = client.request_counted("text");
        let elapsed = (client.clock)(&client.io).duration_since(client.io.base);
        assert!(
            matches!(got.as_ref(), Err(e) if e.kind() == std::io::ErrorKind::TimedOut),
            "reply {got:?}, controlled elapsed {elapsed:?}, exchange budget {wait:?}"
        );
        assert!(
            client.request_line("status").is_err(),
            "a partial body must poison the connection"
        );
        assert_eq!(client.io.sent, b"text\n");
    }

    fn bounded_fragments(
        io: FragmentTransport,
        wait: std::time::Duration,
    ) -> RelayClient<FragmentTransport> {
        let mut client = RelayClient::new(io);
        client.bound = Some((arm_fragments, wait));
        client.clock = fragment_clock;
        client
    }

    #[test]
    fn fragment_control_per_io_timeouts_accept_a_late_counted_body() {
        let ms = std::time::Duration::from_millis;
        let io = FragmentTransport::new(&[b"OK 1\n", b"r", b"o", b"w\n"], ms(3));
        // Actual old behavior: the transport is armed once, with no total client budget.
        arm_fragments(&io, ms(10)).unwrap();
        let mut legacy = RelayClient::new(io);
        assert_eq!(
            legacy.request_counted("text").unwrap(),
            ("OK 1".into(), "row\n".into())
        );
        assert_eq!(legacy.io.now.get().duration_since(legacy.io.base), ms(12));
    }

    #[test]
    fn fragment_deadline_checks_late_operations_and_byte_bodies() {
        let ms = std::time::Duration::from_millis;
        for round_up in [false, true] {
            let mut io = FragmentTransport::new(&[b"OK 4\n", b"a", b"b", b"cd"], ms(3));
            io.round_up = round_up;
            let mut client = bounded_fragments(io, ms(10));
            assert_eq!(
                client.request_bytes("image --bytes").unwrap_err().kind(),
                std::io::ErrorKind::TimedOut
            );
            assert!(client.request_frame("feed-bin", b"later").is_err());
            assert_eq!(client.io.sent, b"image --bytes\n");
        }
        let io = FragmentTransport::new(&[b"OK 2\n", b"\0", b"\xff", b"OK next\n"], ms(2));
        let mut client = bounded_fragments(io, ms(10));
        assert_eq!(
            client.request_bytes("image --bytes").unwrap(),
            ("OK 2".into(), vec![0, 255])
        );
        assert_eq!(client.request_line("status").unwrap(), "OK next");
    }

    #[test]
    fn fragment_deadline_includes_partial_writes_and_flush() {
        let ms = std::time::Duration::from_millis;
        for frame in [false, true] {
            let mut io = FragmentTransport::new(&[b"OK done\n"], ms(3));
            io.write_chunk = 2;
            io.write_step = ms(2);
            io.flush_step = ms(2);
            let mut client = bounded_fragments(io, ms(10));
            let got = if frame {
                client.request_frame("feed-bin", b"x")
            } else {
                client.request_line("status")
            };
            assert_eq!(got.unwrap_err().kind(), std::io::ErrorKind::TimedOut);
            assert!(!client.io.sent.is_empty());
            let sent = client.io.sent.clone();
            assert!(client.request_line("later").is_err());
            assert_eq!(client.io.sent, sent);
        }
        let mut io = FragmentTransport::new(&[b"OK done\n"], ms(3));
        io.write_chunk = 4;
        io.write_step = ms(1);
        io.flush_step = ms(1);
        let mut client = bounded_fragments(io, ms(10));
        assert_eq!(client.request_frame("feed-bin", b"x").unwrap(), "OK done");
        assert_eq!(client.io.sent, b"feed-bin 1\nx");
    }

    #[test]
    fn fragment_deadlines_preserve_named_waits_and_reset_between_requests() {
        let ms = std::time::Duration::from_millis;
        let io = FragmentTransport::new(&[b"OK 1\n", b"r", b"o", b"w\n", b"OK next\n"], ms(3));
        let mut client = bounded_fragments(io, ms(10));
        assert_eq!(
            client.request_counted("await idle timeout=10").unwrap(),
            ("OK 1".into(), "row\n".into())
        );
        assert_eq!(client.io.now.get().duration_since(client.io.base), ms(12));
        assert_eq!(client.request_line("status").unwrap(), "OK next");
    }

    #[test]
    fn fragment_stream_reads_keep_transport_timeout_semantics() {
        let ms = std::time::Duration::from_millis;
        let io = FragmentTransport::new(
            &[b"OK\n", b"stream", b" progress", b" remains", b"\n"],
            ms(3),
        );
        let mut client = bounded_fragments(io, ms(10));
        assert_eq!(client.request_line("subscribe").unwrap(), "OK");
        assert_eq!(client.next_pushed().unwrap(), "stream progress remains");
        assert_eq!(client.io.now.get().duration_since(client.io.base), ms(15));

        let io = FragmentTransport::new(&[b"OK\n", b"stream\n"], ms(3));
        let mut client = bounded_fragments(io, ms(10));
        assert_eq!(client.request_line("subscribe").unwrap(), "OK");
        client.transport().timeout.set(ms(1));
        assert_eq!(
            client.next_pushed().unwrap_err().kind(),
            std::io::ErrorKind::TimedOut
        );
    }

    #[test]
    fn malformed_or_cut_bodies_poison_but_never_count_as_auth_close() {
        for bytes in [
            &b"OK 2\none\n"[..],
            &b"OK 1\n\xff\n"[..],
            &b"OK 99999999999999999999999999\n"[..],
        ] {
            let mut client = RelayClient::new(RecordingTransport::new(bytes));
            assert!(client.request_counted("version").is_err());
            assert!(client.reply_unread());
            assert!(!client.closed_before_reply());
            assert!(client.request_line("later").is_err());
            assert_eq!(client.io.sent, b"version\n");
        }
        let mut client = RelayClient::new(RecordingTransport::new(b""));
        assert_eq!(
            client.request_line("version").unwrap_err().kind(),
            std::io::ErrorKind::UnexpectedEof
        );
        assert!(client.closed_before_reply());
    }
    use super::*;

    struct RecordingTransport {
        sent: Vec<u8>,
        to_read: std::io::Cursor<Vec<u8>>,
        max_read: usize,
        error_at: Option<u64>,
    }
    impl RecordingTransport {
        fn new(canned: &[u8]) -> Self {
            Self {
                sent: Vec::new(),
                to_read: std::io::Cursor::new(canned.to_vec()),
                max_read: usize::MAX,
                error_at: None,
            }
        }
    }
    impl Read for RecordingTransport {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            if self.error_at == Some(self.to_read.position()) {
                self.error_at = None;
                return Err(std::io::ErrorKind::WouldBlock.into());
            }
            let len = out.len().min(self.max_read);
            self.to_read.read(&mut out[..len])
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

    /// A request whose reply was left unread leaves the connection refusing every later one,
    /// unsent, so the late reply answers none of them; a refusal that wrote nothing leaves it
    /// as it was.
    #[test]
    fn a_reply_left_unread_answers_no_later_request() {
        let mut client = RelayClient::new(RecordingTransport::new(b"OK late\nOK next\n"));
        client.io.error_at = Some(0);
        assert!(!client.reply_unread());
        let cut = client
            .request_line("status")
            .expect_err("the reply did not come");
        assert!(unanswered(&cut), "{cut:?}");
        assert!(client.reply_unread());
        let refused = client.request_counted("who").expect_err("never sent");
        assert_eq!(refused.kind(), std::io::ErrorKind::NotConnected);
        assert!(
            !unanswered(&refused),
            "a request never sent is no unanswered one"
        );
        assert!(client.request_bytes("image --bytes").is_err());
        assert!(client.request_frame("feed-bin", b"x").is_err());
        assert_eq!(
            client.io.sent, b"status\n",
            "nothing after the unread reply"
        );

        let mut client = RelayClient::new(RecordingTransport::new(b"OK 2\nrow\n")).reads_only();
        let cut = client
            .request_counted("text")
            .expect_err("a body cut short");
        assert_eq!(cut.kind(), std::io::ErrorKind::UnexpectedEof);
        assert!(
            client.request_line("status").is_err(),
            "nothing after a cut body"
        );

        let mut client =
            RelayClient::new(RecordingTransport::new(b"OK 1\nrow\nOK next\n")).reads_only();
        let line = client.request_line("status\nwho").expect_err("two lines");
        assert_eq!(line.kind(), std::io::ErrorKind::InvalidInput);
        let send = client
            .request_line("@s-1 send x")
            .expect_err("a read-only send");
        assert_eq!(send.kind(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(
            client
                .request_counted("text")
                .expect("a refusal left it whole"),
            ("OK 1".to_string(), "row\n".to_string())
        );
        assert_eq!(client.request_line("status").expect("the next"), "OK next");
        assert!(!client.reply_unread());
    }

    #[test]
    fn a_callers_reply_bound_counts_every_line_and_ends_an_oversize_connection() {
        let mut fits = RelayClient::new(RecordingTransport::new(b"OK 2\nab\ncd\n"));
        assert_eq!(
            fits.request_counted_with_limit("text", 6).unwrap().1,
            "ab\ncd\n"
        );
        let mut too_long = RelayClient::new(RecordingTransport::new(b"OK 2\nab\ncd\nOK next\n"));
        assert_eq!(
            too_long
                .request_counted_with_limit("text", 5)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidData
        );
        assert!(too_long.request_line("status").is_err());
        assert_eq!(too_long.io.sent, b"text\n");
    }

    std::thread_local! {
        static ARMED: std::cell::RefCell<Vec<std::time::Duration>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }

    #[test]
    fn unknown_closed_peer_reads_never_certify_an_empty_auth_reply() {
        struct FailedRead(std::io::ErrorKind);
        impl Read for FailedRead {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(self.0.into())
            }
        }
        impl Write for FailedRead {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        for kind in [
            std::io::ErrorKind::Interrupted,
            std::io::ErrorKind::WouldBlock,
            std::io::ErrorKind::InvalidData,
            std::io::ErrorKind::NotConnected,
        ] {
            let mut client = RelayClient::new(FailedRead(kind));
            assert!(!client.closed_peer_without_reply(), "{kind:?} is not EOF");
        }
        for kind in [
            std::io::ErrorKind::UnexpectedEof,
            std::io::ErrorKind::BrokenPipe,
            std::io::ErrorKind::ConnectionReset,
        ] {
            let mut client = RelayClient::new(FailedRead(kind));
            assert!(client.closed_peer_without_reply(), "pre-reply {kind:?}");
        }
        let mut empty = RelayClient::new(RecordingTransport::new(b""));
        assert!(empty.closed_peer_without_reply());
        for bytes in [b"partial".as_slice(), b"OK\n", b"\xff"] {
            let mut client = RelayClient::new(RecordingTransport::new(bytes));
            assert!(
                !client.closed_peer_without_reply(),
                "queued bytes {bytes:?}"
            );
        }
        let mut buffered = RelayClient::new(RecordingTransport::new(b""));
        buffered.buf.push(b'x');
        assert!(!buffered.closed_peer_without_reply());
    }

    #[cfg(unix)]
    #[test]
    fn an_os_disconnect_certifies_no_reply_only_before_any_byte_was_observed() {
        struct Disconnected;
        impl Read for Disconnected {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::from_raw_os_error(libc::ENOTCONN))
            }
        }
        impl Write for Disconnected {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut client = RelayClient::new(Disconnected);
        assert!(client.closed_peer_without_reply());
        client.reply_started = true;
        assert!(!client.closed_peer_without_reply());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn closed_peer_timeout_errors_preserve_the_queued_reply() {
        for errno in [libc::EINVAL, libc::ENOTCONN] {
            let (mut near, mut far) = aterm_uds::kept_out(aterm_uds::CtlStream::pair).unwrap();
            near.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
                .unwrap();
            assert!(
                accept_closed_arm(&near, Err(std::io::Error::from_raw_os_error(errno))).is_err()
            );
            far.write_all(b"OK partial").unwrap();
            far.shutdown(std::net::Shutdown::Both).unwrap();
            drop(far);
            aterm_spec::wait::until_peer_closed(&near);
            assert!(
                accept_closed_arm(&near, Err(std::io::Error::from_raw_os_error(libc::EPERM)))
                    .is_err()
            );
            accept_closed_arm(&near, Err(std::io::Error::from_raw_os_error(errno))).unwrap();
            let mut reply = Vec::new();
            near.read_to_end(&mut reply).unwrap();
            assert_eq!(
                reply, b"OK partial",
                "timeout handling must not discard bytes"
            );
        }
    }

    fn armed(_: &RecordingTransport, wait: std::time::Duration) -> std::io::Result<()> {
        ARMED.with(|a| a.borrow_mut().push(wait));
        Ok(())
    }

    /// A bounded connection arms each exchange with the bound and, on top, the wait the request
    /// names: the first `timeout=` (or `timeout <ms>`) it carries, a typed text's words after it
    /// being text.
    #[test]
    fn a_bounded_exchange_waits_the_bound_and_its_own_timeout() {
        let bound = std::time::Duration::from_millis(100);
        let mut client = RelayClient::new(RecordingTransport::new(&b"OK\n".repeat(4)));
        client.bound = Some((armed, bound));
        ARMED.with(|arms| arms.borrow_mut().clear());
        let ms = std::time::Duration::from_millis;
        for (line, wait) in [
            ("@s-1 status", bound),
            ("@s-1 await agent idle timeout=15000", bound + ms(15_000)),
            (
                "@s-1 turn yield=0.2 idle=1 timeout=10000 the timeout=1 of it",
                bound + ms(10_000),
            ),
            ("await seq 4 timeout 300", bound + ms(300)),
        ] {
            assert_eq!(exchange_wait(line, bound), wait);
            ARMED.with(|arms| arms.borrow_mut().clear());
            assert_eq!(client.request_line(line).expect(line), "OK");
            ARMED.with(|arms| {
                let arms = arms.borrow();
                assert_eq!(arms.first(), Some(&wait));
                assert_eq!(arms.last(), Some(&wait));
                assert!(arms.iter().all(|armed| !armed.is_zero() && *armed <= wait));
            });
        }
        assert_eq!(
            exchange_wait("await seq timeout=1", std::time::Duration::MAX),
            std::time::Duration::MAX,
            "a wait past the clock's range saturates"
        );
    }

    /// A bounded connection whose peer never answers ends the exchange at its bound and hangs
    /// up, the client still held, so the peer reads its caller gone; one whose peer answers
    /// reads the answer.
    #[cfg(unix)]
    #[test]
    fn a_bounded_exchange_ends_at_its_bound() {
        use std::io::BufRead as _;
        let (ours, theirs) = std::os::unix::net::UnixStream::pair().expect("pair");
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut client = RelayClient::new(ours).bounded(std::time::Duration::from_millis(50));
            let cut = client.request_line("status");
            let _ = tx.send((cut.map_err(|e| unanswered(&e)), client));
        });
        let (cut, client) = rx
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the exchange within the hang detector");
        assert_eq!(cut, Err(true), "unanswered");
        assert!(client.reply_unread());
        theirs
            .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
            .expect("the peer's hang detector");
        let mut peer = std::io::BufReader::new(&theirs);
        let mut asked = String::new();
        peer.read_line(&mut asked).expect("the request");
        assert_eq!(asked, "status\n");
        asked.clear();
        assert_eq!(
            peer.read_line(&mut asked).map_err(|e| e.kind()),
            Ok(0),
            "the caller hung up while its client lives"
        );
        drop(client);

        let (ours, mut theirs) = std::os::unix::net::UnixStream::pair().expect("pair");
        theirs.write_all(b"OK answered\n").expect("the answer");
        let mut client = RelayClient::new(ours).bounded(DEFAULT_REPLY_WAIT);
        assert_eq!(
            client.request_line("status").expect("answered"),
            "OK answered"
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
    fn relay_client_bounds_a_line_even_when_the_last_read_contains_its_newline() {
        for (len, accepted) in [(MAX_LINE_BYTES, true), (MAX_LINE_BYTES + 1, false)] {
            let mut reply = vec![b'x'; len];
            reply.extend_from_slice(b"\nOK next\n");
            let mut client = RelayClient::new(RecordingTransport::new(&reply));
            let result = client.request_line("status");
            if accepted {
                assert_eq!(result.expect("the exact content bound").len(), len);
                assert_eq!(client.request_line("status").unwrap(), "OK next");
            } else {
                assert_eq!(
                    result
                        .expect_err("the final newline cannot bypass the cap")
                        .kind(),
                    std::io::ErrorKind::InvalidData
                );
            }
        }
    }

    #[test]
    fn relay_client_keeps_fragmented_utf8_crlf_and_later_lines_separate() {
        let expected = format!("prefix {} end", "α🦀z".repeat(4097));
        let mut reply = expected.as_bytes().to_vec();
        reply.extend_from_slice(b"\r\n\xff\nOK next\r\n");
        for chunk in [1, 3, 8191, 8192] {
            let mut transport = RecordingTransport::new(&reply);
            transport.max_read = chunk;
            let mut client = RelayClient::new(transport);
            assert_eq!(client.next_pushed().unwrap(), expected, "chunk={chunk}");
            assert_eq!(
                client.next_pushed().unwrap_err().kind(),
                std::io::ErrorKind::InvalidData,
                "invalid UTF-8 is confined to its own line, chunk={chunk}"
            );
            assert_eq!(client.next_pushed().unwrap(), "OK next", "chunk={chunk}");
        }
    }

    #[test]
    fn relay_client_resumes_a_fragmented_line_after_a_transport_error() {
        let mut transport = RecordingTransport::new("prefix α🦀\r\nOK next\n".as_bytes());
        transport.max_read = 3;
        transport.error_at = Some(3);
        let mut client = RelayClient::new(transport);
        assert_eq!(
            client.next_pushed().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert_eq!(client.next_pushed().unwrap(), "prefix α🦀");
        assert_eq!(client.next_pushed().unwrap(), "OK next");
    }

    #[test]
    fn relay_client_counted_body_has_one_aggregate_allowance() {
        let mut exact = RelayClient::new(RecordingTransport::new(b"a\r\nb\nOK next\n"));
        assert_eq!(
            exact
                .read_body_lines(2, 4)
                .expect("the normalized body fits"),
            "a\nb\n"
        );
        assert_eq!(exact.read_line().expect("the next reply"), "OK next");

        let mut over = RelayClient::new(RecordingTransport::new(b"a\r\nb\nOK next\n"));
        let error = over
            .read_body_lines(2, 3)
            .expect_err("short rows must share the body allowance");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("aggregate"));
        assert_eq!(over.read_line().expect("no extra row consumed"), "OK next");

        let mut empty = RelayClient::new(RecordingTransport::new(b"\n\n"));
        assert_eq!(empty.read_body_lines(2, 2).unwrap(), "\n\n");
        assert!(empty.read_body_lines(0, 0).unwrap().is_empty());
        assert_eq!(
            empty
                .read_body_lines(MAX_TEXT_LINES + 1, usize::MAX)
                .expect_err("the count is bounded before a read")
                .kind(),
            std::io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn relay_client_refuses_overflowing_counts_instead_of_reading_them_as_status() {
        let mut client = RelayClient::new(RecordingTransport::new(
            b"OK 99999999999999999999999999999999999\nbody\n",
        ));
        assert_eq!(
            client
                .request_counted("text")
                .expect_err("a numeric count overflow")
                .kind(),
            std::io::ErrorKind::InvalidData
        );
        let mut client = RelayClient::new(RecordingTransport::new(
            b"OK 99999999999999999999999999999999999\nbody\n",
        ));
        assert_eq!(
            client
                .request_bytes("cast")
                .expect_err("a numeric count overflow")
                .kind(),
            std::io::ErrorKind::InvalidData
        );
        assert_eq!(reply_count("OK schema=1").unwrap(), None);
        assert_eq!(reply_count("OK 2 incomplete").unwrap(), Some(2));
        assert!(reply_count("OK +99999999999999999999999999999999999").is_err());
        assert_eq!(reply_count("OK +2 incomplete").unwrap(), Some(2));
        assert!(reply_count("OK +").is_err());
        assert_eq!(
            reply_count("OK 99999999999999999999999999999999999x").unwrap(),
            None,
            "an overflowing-looking status token is still not a numeric count"
        );
    }

    #[test]
    fn relay_client_frames_a_payload_byte_for_byte() {
        let mut client = RelayClient::new(RecordingTransport::new(b"OK 9 bytes\n"));
        let reply = client
            .request_frame("feed-bin", b"id=x go\\n")
            .expect("a framed request");
        assert_eq!(reply, "OK 9 bytes");
        assert_eq!(client.io.sent, b"feed-bin 9\nid=x go\\n");

        let mut client = RelayClient::new(RecordingTransport::new(b""));
        let err = client
            .request_frame("feed-bin\nsend evil", b"x")
            .expect_err("a header occupies one line");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert!(client.io.sent.is_empty());
    }

    #[test]
    fn relay_client_reads_the_host_gone_line_as_the_connection_closing() {
        let mut client = RelayClient::new(RecordingTransport::new(
            b"ERR gone pid=4242 sock=/run/aterm/aterm-4242.sock\n",
        ));
        let err = client
            .request_line("@s-abc text")
            .expect_err("not an answer");
        assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof, "{err}");
        assert!(err.to_string().contains("pid 4242"), "{err}");
        let mut client = RelayClient::new(RecordingTransport::new(b"ERR gone fishing\n"));
        assert_eq!(
            client.request_line("text").expect("an answer"),
            "ERR gone fishing"
        );
        let row = "ERR gone pid=4242 sock=/run/aterm/aterm-4242.sock";
        let mut client =
            RelayClient::new(RecordingTransport::new(format!("OK 1\n{row}\n").as_bytes()));
        assert_eq!(
            client.request_counted("text").expect("a counted reply"),
            ("OK 1".to_string(), format!("{row}\n"))
        );
    }

    #[test]
    fn a_silent_ctl_failure_still_says_something() {
        let reply = |code, stdout: &str, stderr: &str| supervise::CtlReply {
            code,
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
        };
        let cases = [
            (
                reply(1, "", "aterm-ctl: ERR no such session\n"),
                &["ERR no such session"][..],
            ),
            (reply(124, "OK timeout\n", ""), &["124", "OK timeout"][..]),
            (reply(3, "", " \n"), &["3"][..]),
        ];
        for (failed, tokens) in &cases {
            let words = failure_words(failed);
            for token in *tokens {
                assert!(words.contains(token), "{words:?} lacks {token:?}");
            }
        }
    }

    #[test]
    fn relay_help_memoizes_only_protocol_success() {
        let mut client = RelayClient::new(RecordingTransport::new(b"OKAY\nOK 1\nfresh\n"));
        assert_eq!(client.help_counted("turn").unwrap().0, "OKAY");
        let fresh = client.help_counted("turn").unwrap();
        assert_eq!(fresh, ("OK 1".to_string(), "fresh\n".to_string()));
        assert_eq!(client.help_counted("turn").unwrap(), fresh);
        assert_eq!(client.io.sent, b"help turn\nhelp turn\n");
    }

    #[test]
    fn a_read_only_connection_refuses_every_mutating_subform() {
        let reads = [
            "@s-1 meta",
            "metrics",
            "metrics percentiles",
            "metrics json",
            "metrics --json",
            "metrics percentiles json",
            "metrics --json percentiles",
            "metrics json percentiles --json",
        ];
        let mutations = [
            "@s-1 meta set attention held",
            "@s-1 meta unset attention",
            "metrics reset",
            "@s-1 metrics reset",
            "metrics reset json",
            "metrics json reset",
            "metrics --json reset",
            "metrics reset --json",
            "metrics json reset --json",
        ];
        let replies = b"OK\n".repeat(reads.len() + mutations.len());
        let mut ro = RelayClient::new(RecordingTransport::new(&replies)).reads_only();
        for read in reads {
            assert!(ro.request_line(read).is_ok(), "{read}");
        }
        let before = ro.io.sent.clone();
        let refused: Vec<_> = mutations
            .iter()
            .map(|request| {
                ro.request_line(request)
                    .is_err_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied)
            })
            .collect();
        let mut plain = RelayClient::new(RecordingTransport::new(b"OK\nOK\n"));
        assert!(plain.request_line("metrics reset").is_ok());
        assert!(plain.request_line("meta unset attention").is_ok());
        assert_eq!(
            (refused, ro.io.sent == before),
            (vec![true; mutations.len()], true),
            "no mutating form writes a byte; ordinary connections retain it"
        );
    }

    #[test]
    fn a_read_only_connection_sends_doc_reads_alone() {
        let reads = [
            "doc ls",
            "doc ls uri=file:///tmp/x.txt",
            "@s-1 doc read 3 lines=1-20",
            "doc read file:///tmp/x.txt",
            "doc log 3 since=2",
        ];
        let writes = [
            "doc apply 3 base=4 len=12",
            "@s-1 doc save 3",
            "doc",
            "doc frob 3",
        ];
        let mut ro =
            RelayClient::new(RecordingTransport::new(&b"OK 0\n".repeat(reads.len()))).reads_only();
        for read in reads {
            assert!(ro.request_line(read).is_ok(), "{read}");
        }
        let sent = ro.io.sent.clone();
        for write in writes {
            let refused = ro.request_line(write).expect_err(write);
            assert_eq!(
                refused.kind(),
                std::io::ErrorKind::PermissionDenied,
                "{write}"
            );
        }
        assert_eq!(ro.io.sent, sent, "no write form wrote a byte");
        let mut plain = RelayClient::new(RecordingTransport::new(b"OK\n"));
        assert!(plain.request_line("doc save 3").is_ok());
    }

    #[test]
    fn a_read_only_connection_sends_the_read_verbs_alone() {
        let reads = [
            "@s-1 text --json",
            "status",
            "@s-1 await seq 7 timeout=0",
            "@s-1 meta",
        ];
        let hands = [
            "@s-1 send hi",
            "@s-1 key enter",
            "@s-1 turn hi",
            "@s-1 signal TERM pid=7",
            "@s-1 lease acquire",
            "@s-1 meta set attention x",
            "@s-1 frobnicate",
        ];
        let mut ro = RelayClient::new(RecordingTransport::new(b"OK\nOK\nOK\nOK\n")).reads_only();
        for r in reads {
            assert!(ro.request_line(r).is_ok(), "{r}");
        }
        let read_bytes = ro.io.sent.len();
        for h in hands {
            let err = ro.request_line(h).expect_err(h);
            assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied, "{h}");
        }
        assert!(ro.request_frame("@s-1 feed-bin", b"x").is_err());
        assert_eq!(ro.io.sent.len(), read_bytes, "nothing written for a hand");

        let mut plain = RelayClient::new(RecordingTransport::new(b"OK\nOK\n"));
        assert!(plain.request_line("@s-1 send hi").is_ok());
        assert!(plain.request_line("@s-1 meta set attention x").is_ok());
    }

    /// [`CtlClient::own_at`] runs `<exe> ctl --sock <sock> <args>` in that
    /// order, and [`own_exe`] names a file that exists (this test binary).
    #[cfg(unix)]
    #[test]
    fn the_own_client_runs_the_ctl_verb_first() {
        use std::os::unix::fs::PermissionsExt as _;
        assert!(own_exe().expect("this binary").is_file());
        let dir = std::env::temp_dir().join(format!("aterm-own-ctl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch");
        let exe = dir.join("aterm");
        std::fs::write(&exe, "#!/bin/sh\necho \"$*\"\n").expect("write the stand-in");
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let own = CtlClient::own_at(&exe, Some("/s.sock".to_string()));
        assert_eq!(
            own.run(&["cursor"]).expect("it runs"),
            "ctl --sock /s.sock cursor\n"
        );
        let plain = CtlClient::new(&exe, Some("/s.sock".to_string()));
        assert_eq!(
            plain.run(&["cursor"]).expect("it runs"),
            "--sock /s.sock cursor\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_ctl_client_is_this_image_even_once_its_file_is_replaced() {
        let me = std::env::current_exe().expect("the test binary");
        let canon = std::fs::canonicalize(&me).expect("the test binary resolves");
        assert_eq!(resolved_exe(me.clone()), canon);
        assert_eq!(own_exe().expect("this image"), canon);
        let gone = me.with_file_name("aterm (deleted)");
        let expect = if cfg!(target_os = "linux") {
            std::path::PathBuf::from("/proc/self/exe")
        } else {
            gone.clone()
        };
        assert_eq!(resolved_exe(gone), expect);
    }
}
