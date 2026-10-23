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

/// The crate's durability flush ([`durable::sync_all`]).
mod durable;

use std::io::{Read, Write};

/// AI connections: one working login per AI CLI, its doctor, and reviews across AIs.
pub mod ai;
/// The `aterm drive` CLI, callable in-process.
pub mod drive_cli;
/// The `aterm fleet` CLI, callable in-process.
pub mod fleet_cli;
/// `aterm fleet init|join|peers|allow|leave`: this machine's membership in a fleet.
#[cfg(unix)]
mod fleet_member;
/// `aterm ctl survey --fleet | --host`: the fleet's peers; `aterm ctl --host <h> …`: another Mac
/// driven over it.
#[cfg(unix)]
pub use fleet_member::{drive_fleet, survey_fleet};
/// The harness core.
pub mod harness;
/// The launcher's half of `aterm --headless --ready-fd <n>`.
#[cfg(unix)]
pub mod ready;
/// The supervisor: read-only classification and the loop the window's own host runs on every
/// agent session.
pub mod supervise;

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

    /// Run `aterm-ctl [--sock S] <args...>` and return the exit code with both streams.
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

/// One persistent, authenticated control connection, the in-process twin of [`CtlClient`].
pub struct RelayClient<S: Read + Write> {
    io: S,
    /// Sends only [`READ_VERBS`] ([`Self::reads_only`]).
    reads_only: bool,
    /// Bytes read past the last consumed line boundary (the next reply's prefix).
    buf: Vec<u8>,
    /// The `OK` replies to `help <verb>` this connection has had ([`Self::help_counted`]).
    help: Vec<(String, (String, String))>,
}

/// The verbs a read-only connection may send ([`RelayClient::reads_only`]).
pub const READ_VERBS: &[&str] = &[
    "text", "screen", "line", "cell", "cursor", "modes", "blocks", "history", "status", "timeline",
    "metrics", "await", "who", "sessions", "help", "meta", "ls", "doc",
];

/// Whether the control request `line` is one [`READ_VERBS`] names, in its read form.
fn is_read_request(line: &str) -> bool {
    let mut words = line.split_whitespace().skip_while(|w| w.starts_with('@'));
    match words.next() {
        Some("meta") => words.next().is_none(),
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

    /// The underlying transport.
    pub const fn transport(&self) -> &S {
        &self.io
    }

    /// Send one control line and read one reply line (`OK …` or `ERR …`), returned verbatim.
    /// # Errors
    /// I/O errors writing the request or reading the reply, or an embedded line terminator.
    pub fn request_line(&mut self, line: &str) -> std::io::Result<String> {
        self.write_line(line)?;
        self.read_reply()
    }

    /// Send one control line and read a counted reply.
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
    /// I/O errors, a body past the line bound, or a stream that ends inside the body.
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

    /// Send one length-framed request.
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

    /// Read one line a server pushed on a connection that is already parked on a stream.
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
        Self::authenticated(aterm_uds::CtlStream::connect(sock_path)?, token)
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
}

/// The help for the `aterm drive` tool.
pub const DRIVE_HELP: &str = "\
aterm drive — the window harness's readers, run by hand.

MENTAL MODEL
    Every Claude Code, agy, Muse Code and Gemini CLI session in an aterm window is
    supervised by default (`status supervisor=aterm-harness@<pid>`) under `[harness]`
    in `aterm.toml` (`aterm help harness`). `aterm drive` exposes that host's readers
    and tape tools from a terminal: `classify`, `phase`, `answer`, `ledger`, `tape`,
    and `replay`. Vendor `codex` publishes `status program_state=` and is not read
    here; `codex-alab` supervises itself.
    Driving a session goes through `aterm ctl` (`aterm ctl help`): spawn a worker with
    `aterm ctl @self spawn agent=claude -`, wait with `aterm ctl await task @<sid>,… since=0`,
    and read its report with `aterm ctl @<sid> result`; or use `text`, `turn`, `image`,
    and `await` (`await agent idle,prompt,question,wall`).
    `aterm ctl @sid meta set questions ask|recommended|long-term|short-term` sets a
    session's question policy (`ask` hands dialogs to a person or `aterm drive answer`;
    `meta unset questions` resumes window answering within 2 s).

USAGE
    aterm drive [--socket PATH] <command>
    aterm drive classify [--allow-python GLOB]... <cmd...>
              | phase [@sid] [--allow-python GLOB]...
              | answer @sid [--box TOKEN] <answer...>
              | ledger [@sid] [--journal FILE] [--since TIME] [--format text|md|html] [--out PATH]
                       [--source anchor|log]
              | tape cat FILE [--text] [--salvage] | tape show FILE --decisions [--salvage]
              | tape stats FILE [--salvage]
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
                       Judge whether <cmd...> is read-only: prints `read-only` (exit 0)
                       or `not-read-only <reason>` (exit 1). Flags precede the command
                       (`--` also ends flags). Parses shell syntax first (comments,
                       heredocs, quotes, expansions, segments) and requires every
                       segment head (looking through `env`/`xargs` wrappers) to be a
                       known read-only program in `/bin` or `/usr/bin` (or a matching
                       `--allow-python GLOB` script without `..`). Refuses file
                       redirects, mutating flags/subcommands (`git push/reset/commit`,
                       `git -c/--output/--ext-diff`, `git grep -O`, `git <cmd> --help`,
                       `sed -i`, `awk` `system(`/redirects/pipes, `sed` `w`/`e`/`-f`,
                       `python3 -c`, `sort -o/--compress-program`, `rg --pre`,
                       `printf -v`, `less +cmd`, `date -s`, `uniq IN OUT`,
                       `find -fprint`), and unsafe env assignments (`PATH`, `HOME`,
                       `GIT_*`, `LD_*/DYLD_*`, pager/editor). `aterm ctl` and
                       `aterm drive` are judged by verb and form (`meta`/`task`/
                       `await task`/`result`/`survey`/`ledger` without `--out` read;
                       `meta set`/`task mark`/`spawn`/`answer` write).
    phase [@sid] [--allow-python GLOB]...
                       Read @sid once using its foreground program's reader (`status
                       program=`, else screen detection) and print:
                         busy | prompt | limited | idle | question
                       Followed by details for that phase:
                         prompt:   `kind`, `command`, `description`, `note <text>`,
                                   `classify` (Bash), `option N …`, and `cancel esc`
                                   or `exit esc`
                         question: `box <token>` (for `answer --box`) and options
                         busy:     `reason <where>: <rule>` (from the live zone/footer)
                         limited:  `message <text>` and `reset <text|->`
                       Appends `survey 0` when a dismissible feedback survey is open
                       (`survey withheld (<why>)` when withheld) and `context <n>%`
                       when the context indicator (`<n>% until auto-compact`) is shown.
                       Codex tabs always read `idle`.
    answer @sid [--box TOKEN] <answer...>
                       Send a choice to @sid's open question dialog under its program's
                       grammar (`status program=`, else screen detection) and fences:
                         Claude Code: option number or exact label; `, `-joined options
                           for multi-select; `submit` on the review tab; `recommended`;
                           or `human` (no keys sent). Moves focus row by row and presses
                           Enter fenced on the target row (never digits, Esc, free-text,
                           chat, or cancel).
                         agy: option number, exact label, `recommended`, or `human`;
                           sends the option digit fenced on the title row.
                         Muse Code: option number, exact label, `recommended`, or
                           `human`; moves focus and presses Enter under screen/person
                           fences.
                       `None of the above`, free-text rows, and policy words
                       (`long-term`, `short-term` when not option labels; use
                       `aterm ctl @sid meta set questions <word>`) are refused.
                       `--box TOKEN` requires `phase`'s current `box` token.
                       Prints:
                         `ANSWERED @sid enters=<k> answer=<what> next=<question|review|none>` (exit 0)
                         `LEFT` (person typed, or `human`; exit 0)
                         `NO-BOX` (exit 1)
                         `REFUSED <why>` (exit 2, nothing typed)
                         `NOT-SERVED <why> (enters=<k>)` (exit 3)
    ledger [@sid] [--journal FILE] [--since TIME] [--format text|md|html] [--out PATH]
           [--source anchor|log]
                       Replay @sid's run on one time axis by joining:
                         * `history` (every `turn` and settlement time)
                         * reply row counts from `offscreen tail=20000 max=20000 screen=1`
                         * `--journal FILE` (`<aterm state>/drive/<sid>.journal.jsonl`:
                           `EVENT`, `APPROVED`, `DISMISSED`, `RECONNECT`, `TIMEOUT`, `EXIT`)
                         * fabric mail (`inbox --peek --meta` and `@self` `timeline` `post` rows)
                       Prints `SUMMARY` and `TIMELINE` (`manager | worker | watcher | fabric`).
                       `--format text|md|html` selects column text (default), Markdown tables,
                       or a self-contained swimlane HTML page (`--out PATH` writes mode 0600
                       and prints PATH; bound to `open_ledger`, ⇧⌘L / Ctrl+Shift+L).
                       `--since TIME` filters by Unix ms or timestamp (`YYYY-MM-DD[THH:MM[:SS]][Z|±HH:MM]`).
                       `--source anchor` (default) anchors process clocks via the control
                       socket's birth time (`~` when unknown; `carried=1` turns omit time);
                       `--source log` joins turns and mail via session log records
                       (`log class=turn` `ledger=` and `log kinds=mail` `inbox=`).
                       With no `@sid`, uses the single session named in `--journal`. Exit 0.
    tape cat FILE [--text] [--salvage] | tape show FILE --decisions [--salvage]
    tape stats FILE [--salvage]
                       Read a recorded `.atape` (binary or `atape-text 1`) without
                       contacting aterm. `cat` lists numbered items (`--text` emits
                       round-trippable text form); `show --decisions` prints each
                       decision's screen, rule, outcome, act, reply, and journal line;
                       `stats` prints byte size, item/decision counts, span, and kB/h.
                       Damaged tapes exit 2 unless `--salvage` keeps valid frames
                       before the damage (torn tails are read up to the tear and noted
                       on stderr).
    tape ls [@sid] | tape forget FILE|@sid...
                       Manage hosted tapes in `<state>/drive/tapes/<sid>/` (recorded by
                       dev builds when `ATERM_HARNESS_TAPE` is set, bounded to 256 MiB
                       and 72 h; `tapes/kept/` and active tapes are preserved). `ls`
                       lists tapes newest first (filtered to `@sid` plus `kept/` when
                       given); `forget` deletes named FILEs or all unkept tapes for
                       `@sid` (failing before any deletion if a target has no tape).
    replay FILE [--until door=N]
                       Deterministically replay FILE's loop offline against its
                       recorded observations and compare every request and output.
                       Prints `REPRODUCED`, `ENDED`, `REACHED` (`--until door=N`),
                       `DIVERGED` at `#N` (`decision`, `output`, or `drift`), or
                       `PANICKED`, plus request counts, acts (`> …`), and lines (`| …`).
                       Exit 0 (reproduced/ended/reached), 1 (diverged), 4 (panicked),
                       or 2 (unreplayable/usage error).
    replay FILE --set k=v... | --policy FILE [--set k=v...] [--until door=N]
                       Counterfactual replay in ACTS MODE under `--policy FILE`
                       (`[harness]` TOML) and/or `--set k=v` overrides (unknown or
                       invalid keys/values are refused). Compares only acts up to the
                       first divergence. Prints `REPRODUCED` (0), `ENDED` (0),
                       `DIVERGED` (1), `STALE` (3, candidate requested an unrecorded
                       read), or `PANICKED` (4); exit 2 on usage/read error.
    replay FILE... --shadow [--set k=v...] [--policy FILE]
                       Re-evaluate every recorded decision on each FILE under this
                       build and the candidate policy (or recorded policy), marking
                       missing inputs under `needs:` and rows after the first difference
                       as `TAINT`. Exit 0 (all matched), 1 (diverged), 2 (unshadowable
                       tape or usage error), or 3 (undecidable point).
    replay --check FILE|DIR... [--set k=v...] [--policy FILE]
                       Corpus check: replay every named tape and `*.atape`/`*.atape.txt`
                       under each DIR in acts mode, printing per-tape verdicts
                       (`REPRODUCED`, `NOT REPLAYED`, etc.) and summary counts.
                       Exit 0, 1 (diverged), 2 (not replayed / no tapes / usage error),
                       3 (stale), or 4 (panicked).
    replay FILE --why --at seq=N|door=N|[YYYY-MM-DDT]HH:MM[:SS]|\"<journal line>\"
                       [--format text|md|html]
                       Explain one decision at `--at` (by screen `seq=N`, effect
                       `door=N`, local timestamp, or journal line): the screen, parsed
                       box, input facts with source records, rule trace, act, reply,
                       journal line, ledger row, and `needs:` gaps. Exit 0, or 2 if no
                       decision matches or FILE cannot be read.
    help               Show this text.

OPTIONS
    --socket PATH   Target aterm control socket (defaults to the hosting instance,
                    else the newest local instance).

EXAMPLES
    aterm drive classify 'git status --short && git pull | tail'
    aterm drive phase @s-1e918c46
    aterm ctl @s-1e918c46 meta set questions ask
    aterm drive answer @s-1e918c46 'Use the existing schema'
    aterm drive ledger @s-1e918c46 --format html --out /tmp/ledger.html";

#[cfg(test)]
mod tests {
    use super::*;

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
