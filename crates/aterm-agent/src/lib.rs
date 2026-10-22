// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// The anchor attributes are `macro_rules!` ones where Cargo resolved no proc macro
// under them (crates/aterm-spec-anchor/anchor_rules.rs).
#![cfg_attr(all(anchor_rules, any(test, debug_assertions)), feature(macro_attr))]

//! aterm-agent: the agent interface layer — the window harness's supervisor
//! ([`supervise`]), its readings and the live upgrade ([`harness`]), the
//! `aterm drive` CLI over them ([`drive_cli`]), the `aterm fleet` CLI
//! ([`fleet_cli`]), the AI connections ([`ai`]), and the two control clients
//! they drive a host through: [`CtlClient`] (this build's own `ctl` verb, one
//! process per request) and [`RelayClient`] (one persistent, authenticated
//! connection).
//!
//! The floor against a self-driven feedback storm, a hard per-session rate
//! limit on self-targeted input injection, lives in the control dispatch path
//! (`aterm-gui::inject_floor`), because any raw control client can drive `@.`
//! in a loop. Cross-session self-amplification is bounded by edges: no
//! connection kind carries `DeriveLoop` (`aterm_session::edge::ConnectionKind::ops`),
//! and an edge-scoped connection never forwards to another instance.
#![cfg_attr(
    test,
    allow(
        clippy::disallowed_types,
        reason = "test code: a stand-in child may inherit descriptors"
    )
)]

/// The crate's durability flush ([`durable::sync_all`]).
mod durable;

use std::io::{Read, Write};

/// AI connections: one working login per AI CLI, its doctor, and reviews across AIs.
pub mod ai;
/// The `aterm drive` CLI, callable in-process.
pub mod drive_cli;
/// The `aterm fleet` CLI, callable in-process.
pub mod fleet_cli;
/// `aterm fleet init|join|peers|allow|leave`: this machine's membership in a
/// fleet.
#[cfg(unix)]
mod fleet_member;
/// `aterm ctl survey --fleet | --host`: the fleet's peers, read over its port
/// (`aterm_ctl::LocalVerbs::fleet_survey`); `aterm ctl --host <h> …`: another
/// Mac driven over it (`aterm_ctl::LocalVerbs::fleet_drive`).
#[cfg(unix)]
pub use fleet_member::{drive_fleet, survey_fleet};
/// The harness core: the pure readings behind the aterm wrapper (the usage
/// view, the disk report, the footer) and the live upgrade and relaunch.
pub mod harness;
/// The launcher's half of `aterm --headless --ready-fd <n>`: wait on the
/// instance's own word that its control socket listens.
#[cfg(unix)]
pub mod ready;
/// The supervisor: read-only classification, prompt parsing, the worker's phase,
/// and the loop the window's own host runs on every agent session.
pub mod supervise;

/// The verb the one binary answers a control request on: `aterm ctl …`.
pub const CTL_VERB: &str = "ctl";

/// This process's own executable, resolved through any argv0 alias link
/// (`aterm-drive` -> `aterm`), for a control client run as `<it> ctl …`
/// ([`CtlClient::own_at`]): the one binary answers `aterm ctl`. No sibling
/// `aterm-ctl` and no `PATH` lookup: the client is always this build's own.
///
/// # Errors
/// The executable could not be read.
pub fn own_exe() -> Result<std::path::PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| {
        format!("cannot find this program's own executable for its ctl client: {e}")
    })?;
    Ok(resolved_exe(exe))
}

/// `exe` resolved through its links ([`own_exe`]). Where it no longer names a
/// file (an update replaced the image under a long-running client, whose path
/// then reads `… (deleted)`), Linux still reaches the running image as
/// `/proc/self/exe`, which answers `ctl` as the front door.
fn resolved_exe(exe: std::path::PathBuf) -> std::path::PathBuf {
    match std::fs::canonicalize(&exe) {
        Ok(path) => path,
        Err(_) if cfg!(target_os = "linux") => std::path::PathBuf::from("/proc/self/exe"),
        Err(_) => exe,
    }
}

/// A client that drives a target aterm by shelling out to a control client —
/// this program's own `ctl` verb ([`CtlClient::own_at`]) — one process per
/// request, reusing the verbs a human would type.
pub struct CtlClient {
    ctl: std::path::PathBuf,
    /// The arguments before `--sock` (the `ctl` verb, for [`Self::own_at`]).
    lead: Vec<String>,
    socket: Option<String>,
}

impl CtlClient {
    /// Build a client. `ctl` is the path to a program that answers as
    /// `aterm-ctl` does; `socket` is an explicit `--sock` path, or `None` for
    /// the flagless resolution.
    pub fn new(ctl: impl Into<std::path::PathBuf>, socket: Option<String>) -> Self {
        Self {
            ctl: ctl.into(),
            lead: Vec::new(),
            socket,
        }
    }

    /// A client run as `<exe> ctl …`: `exe` is [`own_exe`], this build's own
    /// binary, which answers the `ctl` verb.
    pub fn own_at(exe: impl Into<std::path::PathBuf>, socket: Option<String>) -> Self {
        Self {
            lead: vec![CTL_VERB.to_string()],
            ..Self::new(exe, socket)
        }
    }

    /// Run `aterm-ctl [--sock S] <args...>`, returning stdout, or why it failed
    /// ([`failure_words`]).
    pub fn run(&self, args: &[&str]) -> Result<String, String> {
        let reply = self.run_raw(args)?;
        if reply.code == 0 {
            Ok(reply.stdout)
        } else {
            Err(failure_words(&reply))
        }
    }

    /// Run `aterm-ctl [--sock S] <args...>` and return the exit code with both
    /// streams, so the supervisor can tell a `124` timeout and an `ERR usage`
    /// from a failure. `Err` only when the client could not be launched.
    pub fn run_raw(&self, args: &[&str]) -> Result<supervise::CtlReply, String> {
        // `aterm-ctl` is a console image: `spawnfd::command` keeps a supervisor
        // with no console (the window's harness host) from opening a visible
        // console window per request on Windows.
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

/// Why a failed `aterm-ctl` run failed, in one string: its stderr, else (a
/// run that exited non-zero in silence) its stdout line and exit code.
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

/// Max body lines a single `text` reply may contain: a bound on an untrusted
/// `OK <n>` count from a possibly compromised relay peer.
const MAX_TEXT_LINES: usize = 200_000;
/// Max bytes before a control-protocol line's LF, including an optional CR.
const MAX_LINE_BYTES: usize = 1 << 20; // 1 MiB
/// Max retained bytes in one counted reply. The individual row/count bounds
/// alone allow a peer to make the client retain hundreds of gigabytes.
const MAX_REPLY_BYTES: usize = 256 << 20; // 256 MiB

#[inline]
fn is_ok_reply(header: &str) -> bool {
    header == "OK" || header.starts_with("OK ")
}

/// A numeric reply count that does not fit `usize` is an invalid count,
/// never an uncounted status: otherwise its unread body becomes the next
/// request's reply on this persistent connection.
fn reply_count(header: &str) -> std::io::Result<Option<usize>> {
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

/// One persistent, authenticated control connection, the in-process twin of
/// [`CtlClient`]: it speaks the raw control verbs directly, with the same verbs
/// and framing `CtlClient` drives through `aterm-ctl`.
///
/// Generic over any `Read + Write` transport: a local `CtlStream`
/// ([`connect_local`](RelayClient::connect_local),
/// [`connect_local_beside`](RelayClient::connect_local_beside)), or an
/// in-memory one in tests. The live upgrade and the supervisor's persistent
/// transport ([`supervise::RelayCtl`]) ride it.
pub struct RelayClient<S: Read + Write> {
    io: S,
    /// Sends only [`READ_VERBS`] ([`Self::reads_only`]): anything else is
    /// refused before a byte is written.
    reads_only: bool,
    /// Bytes read past the last consumed line boundary (the next reply's prefix).
    buf: Vec<u8>,
    /// The `OK` replies to `help <verb>` this connection has had
    /// ([`Self::help_counted`]): a verb's help is its server build's, fixed
    /// for the connection's life. Bounded by [`HELP_MEMO_MAX`].
    help: Vec<(String, (String, String))>,
}

/// The verbs a read-only connection may send ([`RelayClient::reads_only`]):
/// each observes, and none changes the session, its screen, its leases or its
/// processes. `meta` only bare (its `set`/`unset` change the session's
/// metadata), and `doc` only as `ls`, `read` and `log` (its `apply` and
/// `save` change a document). Anything not named here is refused: an
/// allowlist, so a verb added to the server is no read until it is named.
pub const READ_VERBS: &[&str] = &[
    "text", "screen", "line", "cell", "cursor", "modes", "blocks", "history", "status", "timeline",
    "metrics", "await", "who", "sessions", "help", "meta", "ls", "doc",
];

/// Whether the control request `line` (an optional `@<sid>` selector, then the
/// verb) is one [`READ_VERBS`] names, and a bare `meta` when it is `meta`
/// (a read form of `metrics` and `doc` when it is one of those).
fn is_read_request(line: &str) -> bool {
    let mut words = line.split_whitespace().skip_while(|w| w.starts_with('@'));
    match words.next() {
        // The server's only read form is bare `meta`.
        Some("meta") => words.next().is_none(),
        Some("metrics") => {
            // cmd_metrics accepts a summary or `percentiles`, optionally
            // with either JSON marker anywhere. `reset` changes both the
            // metrics and echo-RTT stores; no other form is admitted here.
            let mut command = words.filter(|w| !matches!(*w, "json" | "--json"));
            matches!(command.next(), None | Some("percentiles")) && command.next().is_none()
        }
        // `doc ls`, `doc read` and `doc log` read documents; `doc apply` and
        // `doc save` write them.
        Some("doc") => matches!(words.next(), Some("ls" | "read" | "log")),
        Some(verb) => READ_VERBS.contains(&verb),
        None => false,
    }
}

/// How many verbs' help one [`RelayClient`] remembers; past it, a verb's help
/// is asked each time.
const HELP_MEMO_MAX: usize = 16;

impl<S: Read + Write> RelayClient<S> {
    /// Wrap an already-connected, authenticated transport. For the local AUTH
    /// handshake use [`connect_local`](RelayClient::connect_local).
    pub fn new(io: S) -> Self {
        Self {
            io,
            reads_only: false,
            buf: Vec::new(),
            help: Vec::new(),
        }
    }

    /// This connection, able to send the read verbs alone ([`READ_VERBS`]):
    /// every other request — an input verb, a signal, a lease, a `meta set`, a
    /// length-framed write — is refused before a byte is written
    /// (`PermissionDenied`). What a look that must have no hands runs on: the
    /// live upgrade's dry run and its watch (`harness::upgrade_drive`).
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

    /// `help <verb>` as [`Self::request_counted`] reads it, asked once per
    /// connection: a connection speaks to one server build for its life. Only an
    /// `OK` reply is kept; an `ERR` or I/O error is asked again. The live
    /// upgrade's typed turn asks `help turn` whether the server takes its
    /// `if-gen=` fence, and asking afresh would add a round trip inside the
    /// window the fence guards.
    ///
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

    /// Read the first line of a reply. A forward's host-gone line
    /// ([`aterm_types::control_verbs::host_gone_line`]) is no answer: the aterm
    /// hosting the session went away and the connection closes, so it reads as
    /// end of file (`UnexpectedEof`, naming the host) and every caller that
    /// rides out a hang-up rides it out too. Body rows are not checked.
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

    /// Read one `\n`-terminated line, returned without its trailing `\r?\n`.
    fn read_line(&mut self) -> std::io::Result<String> {
        loop {
            let end = self.buf.iter().position(|&b| b == b'\n');
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
        self.may_send(line)?;
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

    /// The underlying transport, so a caller can take a second handle on the
    /// connection (`try_clone` on a `CtlStream`). The harness uses it to shut
    /// down a connection parked in an `await` from the thread that decided the
    /// wait is over.
    pub fn transport(&self) -> &S {
        &self.io
    }

    /// Send one control line and read one reply line (`OK …` or `ERR …`),
    /// returned verbatim. The caller decides what the words mean; nothing is
    /// interpreted here.
    ///
    /// # Errors
    /// I/O errors writing the request or reading the reply, and a request
    /// carrying an embedded line terminator.
    pub fn request_line(&mut self, line: &str) -> std::io::Result<String> {
        self.write_line(line)?;
        self.read_reply()
    }

    /// Send one control line and read a counted reply: the header, then the
    /// `n` body lines when the header is `OK <n>` with a numeric `<n>`. A header
    /// that is not counted (`OK schema=1 …`, `ERR …`) returns with an empty
    /// body and no further read.
    ///
    /// # Errors
    /// I/O errors, or a count/body past the client's own reply bounds.
    pub fn request_counted(&mut self, line: &str) -> std::io::Result<(String, String)> {
        let header = self.request_line(line)?;
        let count = reply_count(&header)?;
        let Some(count) = count else {
            return Ok((header, String::new()));
        };
        Ok((header, self.read_body_lines(count, MAX_REPLY_BYTES)?))
    }

    /// The shared collector for text and generic counted replies. Charge the
    /// normalized trailing newline too, so even empty rows consume the bound.
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

    /// Send one control line and read a byte-framed reply: the header, then
    /// exactly `n` raw bytes when the header is `OK <n>` and `<n>` is a
    /// number (`inbox get`, `cast`). A header that is not counted returns
    /// with an empty body and no further read, as [`Self::request_counted`].
    ///
    /// # Errors
    /// I/O errors, a body past the line bound, or a stream that ends inside
    /// the body.
    pub fn request_bytes(&mut self, line: &str) -> std::io::Result<(String, Vec<u8>)> {
        let header = self.request_line(line)?;
        let count = reply_count(&header)?;
        let Some(count) = count else {
            return Ok((header, Vec::new()));
        };
        if count > MAX_LINE_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "reply body exceeds the bound",
            ));
        }
        while self.buf.len() < count {
            let mut tmp = [0u8; 8192];
            let n = self.io.read(&mut tmp)?;
            if n == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "control connection closed inside a reply body",
                ));
            }
            self.buf.extend_from_slice(&tmp[..n]);
        }
        let body: Vec<u8> = self.buf.drain(..count).collect();
        Ok((header, body))
    }

    /// Send one length-framed request — `<head> <len>\n` and then `payload` —
    /// and read its one reply line, returned verbatim. This is the frame
    /// `feed-bin` and `paste-bin` read: the payload reaches the PTY byte for
    /// byte, where an inline `send <text>` has leading `id=`/`if=` tokens
    /// parsed out of it and a trailing literal `\n` turned into CR.
    ///
    /// # Errors
    /// I/O errors, and a `head` carrying a line terminator.
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
        let mut frame = format!("{head} {}\n", payload.len()).into_bytes();
        frame.extend_from_slice(payload);
        self.io.write_all(&frame)?;
        self.io.flush()?;
        self.read_reply()
    }

    /// Read one line a server pushed on a connection that is already parked
    /// on a stream (`subscribe`). Nothing is written.
    ///
    /// # Errors
    /// I/O errors, and `UnexpectedEof` when the stream ends.
    pub fn next_pushed(&mut self) -> std::io::Result<String> {
        self.read_line()
    }
}

impl RelayClient<aterm_uds::CtlStream> {
    /// Connect to a local control socket and authenticate: `connect(sock)`, then
    /// a bare `AUTH <token>\n` (acknowledged silently).
    ///
    /// # Errors
    /// I/O errors connecting to `sock_path` or writing the auth line.
    pub fn connect_local(sock_path: &str, token: &str) -> std::io::Result<Self> {
        Self::authenticated(aterm_uds::CtlStream::connect(sock_path)?, token)
    }

    /// [`connect_local`](RelayClient::connect_local) with the instance token
    /// beside the socket, read only after the connect succeeds
    /// ([`aterm_ctl::connect_then_read_token`]): a token read earlier can belong
    /// to a listener an update's successor has since replaced.
    ///
    /// # Errors
    /// I/O errors connecting to `sock_path` or writing the auth line; a token
    /// that cannot be read, prefixed `token: ` and naming the file.
    pub fn connect_local_beside(sock_path: &str) -> std::io::Result<Self> {
        let (stream, token) = aterm_ctl::connect_then_read_token(sock_path)?;
        let token = token.map_err(|e| std::io::Error::new(e.kind(), format!("token: {e}")))?;
        Self::authenticated(stream, &token)
    }

    /// Present `token` on a connection just made: the bare `AUTH` line the
    /// server acknowledges silently.
    fn authenticated(stream: aterm_uds::CtlStream, token: &str) -> std::io::Result<Self> {
        let mut client = Self::new(stream);
        client.write_line(&format!("AUTH {token}"))?;
        Ok(client)
    }
}

/// The help for the `aterm drive` tool, written so a model reading `--help` in
/// a tool result knows what each command reads and prints, and where driving
/// went (`aterm ctl`), without external docs.
pub const DRIVE_HELP: &str = "\
aterm drive — the window harness's readers, run by hand.

MENTAL MODEL
    Every Claude Code, agy, Muse Code and Gemini CLI session in an aterm window is
    ALREADY supervised by the window's own host, by default (`status
    supervisor=aterm-harness@<pid>`), under aterm.toml's [harness] table: `aterm help harness` lists its keys and what
    it does — every box its answer, a question its `answer_text`, a turn that ended
    its continuation, a wall its retry, its reset or a relaunch. These commands are
    that host's own knowledge, run from a terminal: whether a shell line is a read
    (classify), what a worker's screen says (phase), a choice typed into its
    question dialog (answer), and how its run went (ledger); `tape` and `replay`
    read and run again a harness tape a hosted loop recorded. A vendor `codex` is
    shown (its own OSC 7501 report is `status program_state=`), not supervised;
    codex-alab supervises itself and the window only watches it.
    Driving a session goes through `aterm ctl` (`aterm ctl help`): a Claude Code
    WORKER is started on a task with `aterm ctl @self spawn agent=claude -` (the
    task on stdin), waited on with `aterm ctl await task @<sid>,… since=0` run as a
    background command (a later wait names `@<sid>:<rev>`), and read with
    `aterm ctl @<sid> result`; any session is read with `aterm ctl @<sid> text`,
    typed to with `turn`, captured with `image`, and waited on with `await`
    (`await agent idle,prompt,question,wall` for an agent).
    One session's `aterm ctl @sid meta set questions ask|recommended|long-term|short-term`
    decides that session's questions over the table: a priority word answers its
    dialogs and the questions it asks in prose by that priority; `ask` hands each
    of its DIALOGS to a person or a controller (`answer` below), and a dialog it
    holds is answered within 2 s of `meta unset questions`.

USAGE
    aterm drive [--socket PATH] <command>
    aterm drive classify [--allow-python GLOB]... <cmd...>
              | phase [@sid] [--allow-python GLOB]...
              | answer @sid [--box TOKEN] <answer...>
              | ledger [@sid] [--journal FILE] [--since TIME] [--format text|md|html] [--out PATH]
                       [--source anchor|log]
              | tape cat FILE [--text] [--salvage] | tape show FILE --decisions | tape stats FILE
              | tape ls [@sid] | tape forget FILE|@sid...
              | replay FILE [--until door=N]
              | replay FILE --set k=v... | --policy FILE [--set k=v...] [--until door=N]
              | replay FILE... --shadow [--set k=v...] [--policy FILE]
              | replay --check FILE|DIR... [--set k=v...] [--policy FILE]
              | replay FILE --why --at seq=N|door=N|[YYYY-MM-DDT]HH:MM[:SS]|\"<line>\"
                       [--format text|md|html]
              | help

COMMANDS
    classify [--allow-python GLOB]... <cmd...>
                       Is this shell line READ-ONLY, the way the supervisor judges
                       it? Prints `read-only` (exit 0) or `not-read-only <reason>`
                       (exit 1). The flags come first; from the command's first
                       word on, every word is the command's (`classify git log
                       --oneline` works unquoted; `--` also ends the flags). The
                       line is read as the shell reads it first: a `#` comment
                       ends at its newline, a here-document body is data (one
                       under an unquoted terminator that runs `$(…)` refuses),
                       and `$'…'` with an escape, a `${…}` holding a quote or a
                       substitution, and an unterminated quote all refuse. Then
                       quoted strings are dropped, and every segment's HEAD must
                       be a known read-only program (a program word elsewhere is
                       data: `grep -rn open src` reads); a program named by a
                       path outside /bin and /usr/bin is unknown; a wrapper is
                       seen through (`xargs touch`, `env FOO=1 ./x.sh` are touch
                       and ./x.sh), a shell keyword is one only where a command
                       begins (`X=1 if …`, `2>/dev/null for …` and `env if …`
                       run a program by that name), a `&` ends a segment like
                       `;`, git needs a read-only subcommand and form (`git
                       branch NAME` creates). Anywhere on the line, these
                       refuse: git
                       push/reset/commit…, `git -c`, `git --output`,
                       `--ext-diff`, `git grep -O` in any spelling (`-nO`,
                       `--open`), `git <cmd> --help` (it runs the configured
                       man viewer), a redirect to a file (`>&file`
                       too), sed -i, python3 -c, sort -o and
                       --compress-program, rg --pre, printf -v, less +cmd,
                       date -s, uniq IN OUT, find -fprint, and an assignment
                       to PATH (unless every entry is a system bin dir), HOME,
                       GIT_*, LD_*/DYLD_*, a pager or an editor. `aterm ctl`
                       reads only by verb AND form (`meta` and `task` read,
                       `meta set` and `task mark` write; `await task`,
                       `result` and `survey` read, `spawn` writes), and so does
                       `aterm drive` (`ledger` reads unless `--out` writes a
                       file; `answer` writes). python3 runs only a script on the
                       --allow-python globs (none by default; no `..`). The
                       programs handed to awk and sed are read from the raw
                       words: `system(`, a redirect or a pipe in awk, a `w`/`e`
                       command or `s///w` flag in sed, a `-f` program file, all
                       refuse. A tie breaks toward not-read-only.
    phase [@sid]       One read, one word: busy | prompt | limited | idle |
                       question. The worker is read by its own program's
                       grammar (`status program=`, else what its screen shows:
                       Claude Code's, agy's or Muse Code's), as the
                       window's loops read it. For a prompt the parsed box
                       follows: `kind`, `command`, `description`, one `note
                       <text>` per note row (a Bash box's warning rows, e.g.
                       the critical-path
                       `rm` warning), `classify` (a Bash box), one `option N …`
                       per option, then what Esc does — `cancel esc` (it
                       leaves the box), `exit esc` (it quits the program: a
                       trust dialog's), no line where nothing says — and
                       for a question `box <token>`, its token for `answer
                       --box`. For
                       busy, `reason <where>: <rule>` names the signal that
                       fired. For limited, `message <text>` and `reset
                       <text|->`. aterm reads no Codex screen (its Codex
                       screen lane was retired on 2026-10-16): on a Codex
                       every read is `idle` with no evidence and names no box
                       (`status agent=unknown`; its own state is `status
                       program_state=`), so phase and answer read nothing
                       there. What follows describes Claude Code's grammar.
                       A prompt wins over busy, busy over limited,
                       limited over question — except a busy that is only a
                       background monitor: a question, a limit notice or any
                       other wall the turn ended on (an API error, a lost login,
                       a full context; read idle) outranks it (a persistent
                       monitor can outlive any budget).
                       Busy is read from the LIVE ZONE around Claude Code's
                       composer, which sits between two full-width rules. The
                       STATUS ROW is, walking up from the top rule, the first row
                       that starts in column 0 with a spinner glyph, found before
                       any transcript row (the worker's `⏺` message or tool call,
                       output under the `⎿` gutter); what sits between it and the
                       rule never hides it (a tip, a todo list, a hint of any
                       length, a rating survey, a banner, a queued message).
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
                       signal, and so is anything above Claude Code's launch
                       card or another composer's rule: a dead process's frame
                       left over a new one (`/exit`, then `claude` with no
                       `clear`) is not read as live. With no composer rules on
                       the screen every row is scanned. The footer does not always say `esc to interrupt`
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
                       A last line `survey 0` follows when one of Claude Code's
                       rating surveys is OPEN above the composer — `● How is
                       Claude doing this session?` in column 0 (or the
                       long-context, plugin or memory card's own question row)
                       over `1: Bad … 0: Dismiss`, only blank rows, a
                       right-aligned hint or a tip between it and the top rule
                       (a copy quoted in the transcript is not open). `0` is
                       the key that dismisses it: `aterm ctl @sid key
                       'if=^●.How.is.Claude.doing' 0` — the guard matches only
                       a row that starts with `●` (the line for another kind
                       names THAT card's own question row), so a copy quoted in
                       the transcript (indented, or under `⎿`) lets no `0`
                       through to the composer (quoted: `^` is a glob in zsh
                       with extended_glob). While it is open, a turn whose first
                       character is 1, 2 or 3 is taken as a rating — the human's
                       to give, never yours. Dismiss it first. The card is one
                       parked block, so a busy worker still reads busy under
                       it; the transcript-share question, the web-setup offer
                       and the drafted-feedback card are read the same way and
                       are never pressed. A card the loop would not press (a shape
                       nobody has words for that lacks the vendor's `(optional)`
                       mark, or a question row no guard can fence) prints
                       `survey withheld (<why>)` instead: read it with `aterm ctl
                       @sid text`; a `0` for it is a person's call.
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
                       in which the rules you gave it may not survive.
    answer @sid [--box TOKEN] <answer...>
                       Type YOUR choice into the worker's question, read by its
                       own program's grammar (`status program=`, else what its
                       screen shows). Claude Code's question dialog — the
                       screen on display, one tab at a time: an
                       option's number or exact label; for a multi-select the
                       options to check, `, `-joined (each whose check differs
                       is toggled, then its button); `submit` on the review
                       tab; `recommended` (what the window presses under the
                       `recommended` priority; on a multi-select the whole tab:
                       each recommended option checked, then its button);
                       `human` (nothing typed). A free-text answer is refused:
                       that row is a person's (so is a multi-select whose row
                       holds text, before any key); so are `long-term` and
                       `short-term` where no option is labelled so — the
                       window's own policy words: the refusal names `aterm ctl
                       @sid meta set questions <word>`, which has the window
                       answer by it. The keys are the window's own —
                       the focus moved a row a key, each move seen landing,
                       then Enter guarded on the chosen row under the screen
                       fence; never a digit, Esc or a key on the free-text,
                       chat or cancel row. agy's question (read by its
                       answers' roles): an answer's number or exact label,
                       `recommended` (the window's own: its `(Recommended)`
                       answer, else the first) or `human`; the key is the
                       answer's digit, guarded on the question's title row
                       under the screen fence (a host without the fence:
                       NOT-SERVED, nothing typed) — never Enter or Esc, and
                       never a `None of the above`, which asks for a person's
                       own words. Muse
                       Code's question (read by its answers' roles too) takes
                       no digit: the focus is moved to the answer, each move
                       guarded on the question's own row and seen landing,
                       then Enter on the answer's row, every key under the
                       screen fence and the person fence; never Esc, and never
                       `None of the above`. --box TOKEN (`phase`'s `box`) refuses another
                       question, nothing typed. Take a worker's questions
                       with `aterm ctl @sid meta set questions ask` (its window
                       then raises each instead of answering it). Prints one
                       line: `ANSWERED @sid enters=<k> answer=<what> next=<the
                       question now on display|review|none>` (exit 0; <k> the
                       answering keys: the Enters, agy's one digit, Muse
                       Code's one Enter), `LEFT`
                       (a person keyed the session, or `human`; exit 0),
                       `NO-BOX` (exit 1), `REFUSED <why>` (exit 2, nothing
                       typed) or `NOT-SERVED <why> (enters=<k>)` (exit 3).
    ledger [@sid] [--journal FILE] [--since TIME] [--format text|md|html] [--out PATH]
           [--source anchor|log]
                       How the loop RAN, replayed on one time axis: what you
                       sent, what the watcher decided, how big the worker's
                       replies were, and the fabric mail in between. It joins
                       four sources, and names the ones it could not read:
                         * the worker's turn ledger (`history`): every turn, its
                           start, and how the `turn` verb settled (`settled in
                           1.8s`, `timeout after 6.0s`);
                         * the size of the reply each turn drew, in rows, from
                           ONE `offscreen tail=20000 max=20000 screen=1` read,
                           the archived rows joined with the screen's — the rows
                           from the turn's `❯` row to the next turn's (a turn
                           whose own row is nowhere, queued or lost to a
                           restart, is named in the count that holds its reply);
                         * a supervisor's journal, --journal FILE, when given
                           (one JSON object per line; the window's own host keeps
                           one per session, <aterm state>/drive/<sid>.journal.jsonl):
                           every EVENT, APPROVED, DISMISSED, RECONNECT, TIMEOUT
                           and EXIT line, with the wall-clock time it was written;
                         * this session's fabric mail with that worker — the
                           `inbox --peek --meta` rows from it (nothing is listed
                           or handled) and the `post` rows of this session's
                           `timeline` addressed to it.
                       SUMMARY counts the turns, the worker's busy time, your
                       response latency (median and max from each EVENT idle or
                       question to the next turn's start), approvals,
                       dismissals, context warnings and compactions,
                       reconnects, and mail in and out. TIMELINE is one row
                       per item in time order: time, lane (manager | worker |
                       watcher | fabric), what, and the duration or latency.
                       --format text (the default) aligns columns, md writes
                       tables, html writes ONE
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
                       socket. Without that the times are marked `~` and no
                       latency is claimed, and a turn an aterm self-update
                       carried from an earlier process (`carried=1`) is on that
                       process's clock and has no time at all. --source log
                       reads no clock: each turn is placed by its own session
                       log record (`log class=turn`, joined to `history` by
                       `ledger=` alone, never by time; a turn whose open was
                       evicted by its close, which restates `ledger=`) and each
                       message by its `mail` mark (`@self log kinds=mail`,
                       joined by `inbox=`); SOURCES counts what the log no
                       longer holds. --source anchor, the default, is the
                       birth-time path above. It only reads:
                       exit 0 even when a source was missing.
    tape cat FILE [--text] [--salvage] | tape show FILE --decisions | tape stats FILE
                       Read a harness tape a hosted loop recorded (`tape ls`
                       lists them), from the file alone: no aterm is asked
                       anything. `cat`
                       prints its items one line each, every value numbered
                       by its place on the tape; --text prints the exact text
                       form instead — one record per line, every body line
                       by line, a final newline its last empty line — which
                       reads back to the very same tape (a binary or a text
                       tape reads with every verb). `show --decisions`
                       answers \"why did it press 2?\" without a replay: per
                       decision, the screen it judged (the rows of the very
                       read it decided on), the rule and the outcome, the
                       act that followed and its reply, and the line the
                       loop journaled or printed. `stats` prints what the
                       tape cost: its bytes, its items by kind and by door,
                       its decisions, its span and kB per hour of it. A
                       damaged tape is refused, naming the byte, unless
                       --salvage keeps its whole frames before the damage:
                       a frame changed, or, on a tape of frame format 2,
                       whole frames repeated, moved, or dropped from inside
                       it (a format-1 tape cannot tell). A tape that lost
                       its last frames reads as a run that stopped there,
                       with no end: line. A torn tail (a run that died
                       mid-write) is read up to the tear and said on stderr.
                       Exit 0, or 2 for a tape that cannot be read or a
                       usage error.
    tape ls [@sid] | tape forget FILE|@sid...
                       The window's own loops' tapes: a development build
                       records each hosted run when ATERM_HARNESS_TAPE is set
                       (the lab's instances set it; a shipped build never
                       records one), to <state>/drive/tapes/<sid>/, each tape
                       0600 in 0700 directories, the newest kept within
                       256 MiB and 72 h (tapes/kept/ is yours and never
                       swept, nor is a tape a running loop still writes).
                       `ls` lists them newest first (one session's
                       with @sid, the kept ones beside it): size, age, path.
                       `forget` removes each FILE named and every tape of
                       each @sid but its kept ones; anything named that is no
                       tape in the store (a FILE, or an @sid with none)
                       refuses the whole call. Only the store is read or
                       changed.
    replay FILE [--until door=N]
                       Run the loop a harness tape recorded again, offline and
                       exactly: rebuilt from the tape's own setup and policy
                       (every file it named moved under a directory that does
                       not exist), fed every value the recorded loop observed
                       in its order, every request and line it makes checked
                       against the recorded one where it occurs. Only the
                       file is read: no socket, no session, no clock, no file
                       the run named; a journal or ledger row is compared,
                       never written. Prints REPRODUCED, ENDED (a tape with no
                       end — a killed run, a tear, the cap — reproduced as far
                       as it goes), REACHED (--until door=N: stopped after the
                       value numbered N in `tape cat`), DIVERGED at #N with
                       its door and class (decision: an act or a decision
                       differs; output: a line, row or write differs; drift:
                       anything else) and why, or PANICKED (the recorded panic
                       reproduced, or the door it died in reached), then how
                       many requests the replayed loop asked, how it ended,
                       each act it
                       made (`> key 1`: a key, send, post or signal; its reads
                       are only counted) and each line it printed (`| …`).
                       Strict: the build that recorded the tape replays it; on
                       another build a divergence is behaviour changed since
                       the recording's, and says both builds. A hosted tape
                       replays a window host's every answer from the tape (no
                       host is asked, its notifications compared). Exit 0 (reproduced, ended,
                       reached), 1 (diverged), 4 (panicked), or 2 for a tape
                       that cannot be replayed or a usage error.
    replay FILE --set k=v... | --policy FILE [--set k=v...] [--until door=N]
                       \"Would it have pressed the same key under another
                       policy?\": a COUNTERFACTUAL — the recorded loop run
                       again under a CANDIDATE policy, up to the first act it
                       takes otherwise. `--policy FILE` reads a whole
                       [harness] table (an aterm.toml) by the owner's reader
                       in place of the recorded policy; each `--set k=v` (a
                       [harness] key, `harness.` before it or not) sets one
                       key over it; a key or a value the reader refuses — in
                       a --set or anywhere in the file, a misplaced key too —
                       is refused, never run as a default. ACTS MODE:
                       only the ACTS are held to the tape — a key, send,
                       post, badge, signal, restart or the window host's act
                       must be the next one the recorded run took, byte for
                       byte; a read is answered from its own stretch of the
                       tape between two acts (skipping ahead within it, or
                       the same answer again), a derived look from before
                       the point, never from past an act not yet taken; a
                       clock with no record there runs on the recorded
                       stamps and the sleeps; outputs, decisions and the
                       claim's renewals and release are not compared.
                       Prints REPRODUCED (every act taken as recorded),
                       DIVERGED at the first act taken otherwise — the
                       recorded act and the candidate's (none where it ended
                       without it, or went on to wait for the screen the
                       recorded run waited for only after it), the
                       decisions before each, the
                       last screen the candidate read — STALE (the candidate
                       asks a read the tape never answered there: what it
                       would have seen is not on the tape; the recorded
                       run's next act said), or ENDED (a tape with no end,
                       every record on it taken), then how far it strayed
                       and the BOUND: nothing past the first divergent act,
                       no agent re-driven. Exit 0, 1 (diverged), 3 (stale),
                       4 (panicked), or 2.
    replay FILE... --shadow [--set k=v...] [--policy FILE]
                       Every decision on each tape decided again by THIS
                       build under the candidate (the recorded policy with
                       neither): an approval or a question on any build,
                       from the facts its mark carries and the looks the
                       tape holds before it; a turn end on the recording
                       build only, beside a strict replay that rebuilds its
                       fold (on another build each says why not). One row
                       per decision, both sides, `needs:`, and TAINT on
                       every row after the first that differs (the candidate
                       would not have seen those screens). Exit 0 (every
                       point decided again, and as recorded), 1 (one decided
                       otherwise), 3 (none otherwise, but one not decided
                       again: the tape cannot say there), or 2. Over several
                       tapes each is said under its own heading — one that
                       does not shadow with why — and the exit is 1 if any
                       point is decided otherwise, else 2 if a tape did not
                       shadow, else 3, else 0.
    replay --check FILE|DIR... [--set k=v...] [--policy FILE]
                       A CORPUS CHECK: every tape named, and every tape
                       (*.atape, *.atape.txt) under each DIR at any depth,
                       replayed in ACTS MODE as above under its own recorded
                       policy (or the candidate's): does this build still
                       take every act the recorded run took? One line per
                       tape — REPRODUCED with the acts it took, or its whole
                       report under it — then the count of each verdict.
                       aterm's development checkout keeps a corpus of such
                       tapes, recorded in its lab over a real agent and
                       scrubbed of the recording machine's names, and its
                       tests check every one. Exit 0 (every tape's
                       acts taken), else 1 if one diverged, else 2 if one was
                       not replayed (each still said), else 3 (stale), else 4
                       (its recorded panic); 2 too for a check that finds no
                       tape or a usage error.
    replay FILE --why --at seq=N|door=N|[YYYY-MM-DDT]HH:MM[:SS]|\"<journal line>\"
                       [--format text|md|html]
                       \"Why did it press 2?\": one decision on the tape
                       explained instead of the whole run replayed — the
                       screen it judged, the box this build reads on it, the
                       FACTS it was decided on and the record each came from
                       (`ctl meta #12`, `probe.worker_proc #30`), the
                       policy's RULE TRACE (every check consulted, the rule
                       that decided), the act, its reply, the journal line
                       and the ledger row. An approval or a question is
                       decided again by THIS build's policy on the facts the
                       decision's mark carries, the policy the tape's header
                       carries and the looks the tape holds before it (never
                       one from after), labelled with both builds and whether
                       this build's policy decides the same; a fact or a look
                       the tape lacks, or a policy value this build supplied
                       where the header does not say the run's, is listed
                       under `needs:`, never presented as the recorded
                       world. A turn end is traced by a strict replay of the
                       tape to the point, on the build that recorded it only
                       (it decides on a fold over the whole run); on another
                       build its note of the fold and reading is printed
                       instead. --at names the point:
                       `seq=N` (the decision on that screen), `door=N` (the
                       decision in force at effect #N of `tape cat`),
                       `[YYYY-MM-DDT]HH:MM[:SS]` (the last at or before that
                       local time, each decision read by the offset its own
                       instant had; with no day named, the one day of the
                       run the time falls within — a time on more than one
                       is refused, asking for the day), or a journal line's
                       text (the decision it followed). `tape ls @sid` names
                       a session's tapes. Only the file is read (and the
                       local zone's offsets, for a local time), and no
                       running loop ever keeps a trace. Exit 0, or 2 for a
                       point with no decision, a tape that does not read, or
                       a usage error.
    help               Show this text.

OPTIONS
    --socket PATH   The target aterm's control socket. Defaults to the
                    instance hosting this terminal, else the newest local
                    instance.

EXAMPLES
    # is this line a read? (exit 0/1; the reason names the rule)
    aterm drive classify 'git status --short && git pull | tail'
    # what is the worker doing right now?
    aterm drive phase @s-1e918c46
    # take its questions, then answer the one on display by its label:
    aterm ctl @s-1e918c46 meta set questions ask
    aterm drive answer @s-1e918c46 'Use the existing schema'
    # how its run went, as one page in the browser:
    aterm drive ledger @s-1e918c46 --format html --out /tmp/ledger.html";

#[cfg(test)]
mod tests {
    use super::*;
    // --- RelayClient (the persistent connection) ---

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

    /// A framed request carries its payload byte for byte after a header that
    /// counts it, and reads one reply; a header with a line terminator is
    /// refused before anything is written.
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

    /// A forwarding aterm's last line, `ERR gone pid=<host> sock=<its socket>`,
    /// is the same fact as a direct connection's EOF; every read sees it as the
    /// connection closing, naming the host.
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
        // A refusal that only starts alike is still an answer.
        let mut client = RelayClient::new(RecordingTransport::new(b"ERR gone fishing\n"));
        assert_eq!(
            client.request_line("text").expect("an answer"),
            "ERR gone fishing"
        );
        // Only where a reply begins: a screen row that reads alike is a row.
        let row = "ERR gone pid=4242 sock=/run/aterm/aterm-4242.sock";
        let mut client =
            RelayClient::new(RecordingTransport::new(format!("OK 1\n{row}\n").as_bytes()));
        assert_eq!(
            client.request_counted("text").expect("a counted reply"),
            ("OK 1".to_string(), format!("{row}\n"))
        );
    }

    /// A failed `aterm-ctl` run is never an empty error: its stderr, else its
    /// stdout line and exit code, else the exit code alone.
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

    /// A read-only connection sends the read verbs as written (`text`, `status`,
    /// `await`, a bare `meta`, and metric reads under either JSON spelling, with
    /// or without a selector); an input verb, a signal, a lease, a `meta set`, a
    /// metric reset, a verb it does not name and a length-framed write are each
    /// refused with nothing written. Negative control: an ordinary connection
    /// sends them.
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
        // Observe every case before the aggregate assertion, so the old
        // transport guard demonstrates each reset spelling rather than only one.
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

    /// `doc` is a read in its read forms alone: `ls`, `read` and `log` are
    /// sent, aimed or not, and `apply`, `save`, an unknown form and the bare
    /// verb write nothing. NEGATIVE CONTROL: an ordinary connection sends a
    /// write form.
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
    /// Negative control: [`CtlClient::new`] puts nothing before `--sock`.
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

    /// The control client is THIS image, resolved through its links. An image
    /// whose file is gone (replaced by an update under a long-running client)
    /// is still reached on Linux as `/proc/self/exe`.
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
