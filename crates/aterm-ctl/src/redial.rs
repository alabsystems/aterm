// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! SURVIVING AN ATERM SELF-UPDATE: a `subscribe` stream and a blocking read
//! follow the instance that replaced the one they were talking to.
//!
//! WHY (code-read and measured 2026-09-24 on the owner's Mac, 0.93.0; six
//! updates in six days): every update replaces the server process and its
//! socket — `aterm.sock -> aterm-7641.sock` was repointed from
//! `aterm-28438.sock` at 20:25 — and the old process `_exit`s at its Commit,
//! which closes every connection it held. `aterm ctl subscribe` then relayed
//! EOF as success (`io::copy` → exit 0), so a pipeline read an update as
//! "session over"; a blocking `await` failed with "server closed the
//! connection without responding", and an agent following CLAUDE.md's
//! `await inbox since=` advice lost its wait without knowing why. Nothing in
//! this client redialled.
//!
//! THE RULE, at a hang-up that the stream itself did not announce:
//!
//! 1. every subscribed session already said `EVENT <local> exited` → the
//!    session is over: exit 0, as before;
//! 2. the SAME server process still ANSWERS on the socket we dialled (its pid,
//!    read off the connection with `LOCAL_PEERPID`/`SO_PEERCRED`, and an `OK` to
//!    `version` — [`serving_pid`] says why accepting is not enough) → it ended
//!    the stream on purpose (a fixed-target subscription whose sessions all
//!    closed): exit 0, as before;
//! 3. otherwise the server is gone. Within [`SUCCESSOR_BOUND`] (the handoff's own
//!    hold bound) a DIFFERENT live server answering where this call's own
//!    resolution now points — the explicit socket a successor rebinds, the
//!    calling session's graph entry (a call made INSIDE a session follows that
//!    and nothing else), or, outside any session, the `latest` alias — is its
//!    successor:
//!    `subscribe` re-sends its request there and keeps relaying, and a blocking
//!    read is asked once more. No successor: exit [`EXIT_REPLACED`] (75), never 0.
//!
//! An EXPLICIT `--timeout` bounds that wait too: it is a wall clock over the
//! whole `subscribe` relay and a deadline on a blocking read, and a hang-up
//! does not extend it (measured by the 2026-09-24 review on an isolated
//! headless instance SIGKILLed 1 s into the call: `--timeout 3 subscribe` and
//! `--timeout 3 await match` each exited 75 after 31 s, the 30 s successor wait
//! on top of the caller's 3). The successor wait is the SMALLER of
//! [`SUCCESSOR_BOUND`] and what the caller's deadline has left.
//!
//! Never a DIFFERENT instance the caller did not reach this way: the dead-alias
//! fallback that a fresh flagless call takes ("the newest instance that answers")
//! is deliberately not a successor, and a `--pid` pin names one process for good.
//! Nor, for a call made inside a session, the `latest` alias (review of
//! 2026-09-25): with two windows up, the alias names the NEWER one, and a
//! self-updating older one's successor republishes its sessions' graph entries
//! only once it has bound — in between, the entry still named the dead socket,
//! the alias answered as the other window, and a blocking read was asked again
//! on that stranger's active tab, usually a person's.

use std::io::{self, BufReader, Read as _};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use aterm_uds::CtlStream;

/// The exit code of an exchange cut by its server going away with no successor
/// answering (an update that did not come back, a crash, a quit): EX_TEMPFAIL's
/// number. Distinct from 0 so a pipeline never reads the loss as "session over",
/// and from 1/124 so it is never read as a refusal or a timeout.
pub(crate) const EXIT_REPLACED: u8 = 75;

/// How long a successor may take to answer: the update handoff's own hold bound
/// (the outgoing process parks at most this long for its candidate to prove).
pub(crate) const SUCCESSOR_BOUND: Duration = Duration::from_secs(30);

/// Between two looks for a successor.
const SUCCESSOR_POLL: Duration = Duration::from_millis(100);

/// Where a successor of the server this call reached may answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Follow {
    /// A socket path the call PINNED (`--sock`): a successor rebinds that
    /// same path.
    Path(String),
    /// Flagless: inside a session, the instance hosting it (its graph entry,
    /// `<dir>/graph/<sid>`) and nothing else; outside any session, the
    /// `latest` alias `<dir>/aterm.sock` — what a successor republishes for
    /// each. A self-update's successor binds a NEW per-instance socket
    /// (`aterm-<its pid>.sock`), so this is the real update path: the socket
    /// the call dialled never answers again.
    Flagless {
        /// The rendezvous dir the call resolved against.
        dir: PathBuf,
        /// `$ATERM_PARENT_SESSION_ID`: the session hosting the caller.
        self_sid: Option<String>,
    },
    /// Nothing follows: a `--pid` pin names one process, which a replacement
    /// is not.
    Nothing,
}

/// The redial policy of one call.
#[derive(Clone, Debug)]
pub(crate) struct Redial {
    follow: Follow,
    bound: Duration,
}

/// What a hang-up turned out to be.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Hangup {
    /// The server we dialled still accepts: it ended the exchange on purpose.
    StillServing,
    /// A different live server answers where this call follows.
    Successor { path: String, pid: u32 },
    /// Nothing answered within `waited` — [`SUCCESSOR_BOUND`], or less when the
    /// caller's own deadline had less left (`by_deadline`); zero when nothing
    /// follows at all.
    Gone { waited: Duration, by_deadline: bool },
    /// This platform cannot read a peer pid, so a replacement cannot be told
    /// from a deliberate end: the call keeps its old behaviour.
    Unknown,
}

impl Redial {
    /// The policy for a call that follows `follow`.
    pub(crate) fn new(follow: Follow) -> Self {
        Self {
            follow,
            bound: SUCCESSOR_BOUND,
        }
    }

    /// A policy that never follows — the redialled leg of a blocking read,
    /// which is retried once.
    pub(crate) fn none() -> Self {
        Self::new(Follow::Nothing)
    }

    /// The same policy with a different successor bound (tests).
    #[cfg(test)]
    pub(crate) fn with_bound(mut self, bound: Duration) -> Self {
        self.bound = bound;
        self
    }

    /// Every path a successor of the server reached at `dialed` may answer
    /// on, in order, resolved now. A call made inside a session follows ONLY
    /// that session's graph entry — not the path it dialled (which may have
    /// been the `latest` alias, when the entry was not there yet) and not the
    /// alias: both can name another live window, and only the entry is
    /// republished by the successor of the instance that hosts the session.
    fn candidates(&self, dialed: &str) -> Vec<String> {
        match &self.follow {
            Follow::Path(path) => vec![dialed.to_string(), path.clone()],
            Follow::Flagless {
                dir,
                self_sid: Some(sid),
            } => super::self_instance_sock_in(dir, Some(sid))
                .into_iter()
                .collect(),
            Follow::Flagless {
                dir,
                self_sid: None,
            } => vec![
                dialed.to_string(),
                dir.join(super::SOCK_FILE).to_string_lossy().into_owned(),
            ],
            Follow::Nothing => Vec::new(),
        }
    }

    /// Classify a hang-up from the server `old` reached at `dialed` (the rule on
    /// this module). Waits for a successor up to the bound, or up to `until` —
    /// the caller's explicit deadline — when that comes first; returns at once
    /// when the old server still serves, or when nothing can follow.
    pub(crate) fn after_hangup(
        &self,
        dialed: &str,
        old: Option<u32>,
        until: Option<Instant>,
    ) -> Hangup {
        let Some(old) = old else {
            return Hangup::Unknown;
        };
        let started = Instant::now();
        let bound = started + self.bound;
        let by_deadline = until.is_some_and(|until| until < bound);
        let deadline = until.map_or(bound, |until| until.min(bound));
        let probe = |path: &str| {
            let left = deadline.saturating_duration_since(Instant::now());
            serving_pid_within(path, left.clamp(PROBE_FLOOR, PROBE_TIMEOUT))
        };
        if probe(dialed) == Some(old) {
            return Hangup::StillServing;
        }
        if self.follow == Follow::Nothing {
            return Hangup::Gone {
                waited: Duration::ZERO,
                by_deadline: false,
            };
        }
        loop {
            for path in self.candidates(dialed) {
                if let Some(pid) = probe(&path)
                    && pid != old
                {
                    return Hangup::Successor { path, pid };
                }
            }
            let now = Instant::now();
            if now >= deadline {
                return Hangup::Gone {
                    waited: now.duration_since(started),
                    by_deadline,
                };
            }
            std::thread::sleep(SUCCESSOR_POLL.min(deadline - now));
        }
    }
}

/// How long a probe waits for `OK` to its `version` — less when the caller's
/// deadline has less left, never less than [`PROBE_FLOOR`].
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// The shortest probe: a live aterm answers `version` in well under this.
const PROBE_FLOOR: Duration = Duration::from_millis(100);

/// [`serving_pid_within`] with the full probe wait (tests).
#[cfg(test)]
pub(crate) fn serving_pid(path: &str) -> Option<u32> {
    serving_pid_within(path, PROBE_TIMEOUT)
}

/// The pid of the process ANSWERING at `path` right now — it accepted a
/// connection AND answered `version` with `OK` within `wait` — or `None`
/// (nothing answers, or the platform cannot read a peer pid).
///
/// A bare connect is not enough, measured 2026-09-24 against a real headless
/// instance: SIGKILLed, it closed the subscriber's connection before its
/// listening socket, and a probe connect in between landed in the dying
/// listener's backlog and read the DEAD process's pid, so the relay took the
/// hang-up for a deliberate end and exited 0. A process that is going cannot
/// answer a request.
fn serving_pid_within(path: &str, wait: Duration) -> Option<u32> {
    let stream = CtlStream::connect(aterm_uds::latest::resolve(path)).ok()?;
    let pid = server_pid(&stream)?;
    let _ = stream.set_read_timeout(Some(wait));
    let _ = stream.set_write_timeout(Some(wait));
    super::send_request(&stream, super::read_token_for(path).as_deref(), "version\n").ok()?;
    let mut reader = BufReader::new(&stream);
    let mut line = String::new();
    super::read_bounded_line(&mut reader, &mut line).ok()?;
    line.starts_with("OK").then_some(pid)
}

/// The pid of the server at the other end of `stream`.
pub(crate) fn server_pid(stream: &CtlStream) -> Option<u32> {
    aterm_uds::fdpass::peer_pid(stream).ok()
}

/// Whether an I/O error means the peer went away (as opposed to a deadline or a
/// local fault).
pub(crate) fn is_hangup(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::UnexpectedEof
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::NotConnected
    )
}

/// Whether a request is retried ONCE on a successor after a hang-up before its
/// reply: a blocking read with no instance-local anchor. `await seq <n>` (a
/// per-grid counter a successor restarts), `await inbox since=<id>` and
/// `inbox get <id>` (inbox row ids: each process's fabric numbers its own rows
/// from its own counter, and a handoff carries none of them — the same id on
/// the successor can name a different message or none) are not — their anchor
/// means nothing on another process, so they end with [`EXIT_REPLACED`] and a
/// note to re-issue them. Nor is any request whose SESSION is named by its local
/// number (`@<n>`, [`is_local_selector`]): a successor numbers the sessions it
/// adopts afresh, in layout order, so `@2` there can be another tab — a person's
/// — and an `await match` asked again could succeed on the wrong one (review of
/// 2026-09-25). The bare `inbox` listing has no anchor: the successor lists what
/// it holds.
pub(crate) fn retries_across_replacement(request_parts: &[String]) -> bool {
    let rest = match request_parts.first() {
        Some(sel) if is_local_selector(sel) => return false,
        Some(sel) if sel.starts_with('@') => &request_parts[1..],
        _ => request_parts,
    };
    let args: Vec<&str> = rest.iter().skip(1).map(String::as_str).collect();
    match rest.first().map(String::as_str) {
        Some("text" | "wait" | "ready") => true,
        Some("inbox") => args.is_empty(),
        Some("await") => match args.first() {
            Some(&"seq") => args.len() < 2 || args[1].contains('='),
            Some(&"inbox") => !args.iter().any(|a| a.starts_with("since=")),
            Some(_) => true,
            None => false,
        },
        _ => false,
    }
}

/// Whether the request is a blocking read at all (so a hang-up before its reply
/// is reported as a replacement rather than as a bare connection error).
pub(crate) fn is_blocking_read(request_parts: &[String]) -> bool {
    let rest = match request_parts.first() {
        Some(sel) if sel.starts_with('@') => &request_parts[1..],
        _ => request_parts,
    };
    matches!(
        rest.first().map(String::as_str),
        Some("text" | "wait" | "ready" | "await" | "inbox")
    )
}

/// Whether `sel` names a session by the process-LOCAL number the server gave it
/// (`@<digits>`, the server's `Selector::Local`), which means nothing to another
/// process: a successor numbers the sessions it adopts afresh.
pub(crate) fn is_local_selector(sel: &str) -> bool {
    sel.strip_prefix('@')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// The request a redialled `subscribe` sends: the original with its resume
/// anchors (`since=`, `since-turn=`, `since-block=`) removed. They name
/// positions in the OLD process — its per-grid content counter, which a
/// successor restarts (the checkpoint a successor adopts does not carry it),
/// and ring ids — so the successor instead sends each target's current state
/// fresh, which is what a consumer treating the new `sub` lines as a new
/// stream expects.
///
/// A target named by its LOCAL number (`@<n>`, [`is_local_selector`]) is
/// another such anchor, and a worse one: the successor numbers its sessions
/// afresh, so `@3` sent there verbatim streamed ANOTHER tab — a person's —
/// under the resumed stream (review of 2026-09-25). Each is rewritten to the
/// session id the stream's own `sub <n> <sid>` line tied it to (`sids`,
/// [`FrameTracker::sids`]); `None` when one was never tied — it cannot be
/// followed, and the relay ends with [`EXIT_REPLACED`].
pub(crate) fn resume_request(request: &str, sids: &[(String, String)]) -> Option<String> {
    let mut out: Vec<String> = Vec::new();
    for token in request.split_whitespace() {
        if token.starts_with("since=")
            || token.starts_with("since-turn=")
            || token.starts_with("since-block=")
        {
            continue;
        }
        if !token.starts_with('@') {
            out.push(token.to_string());
            continue;
        }
        let mut pieces = Vec::new();
        for piece in token.split(',') {
            if is_local_selector(piece) {
                let (_, sid) = sids.iter().find(|(local, _)| *local == piece[1..])?;
                pieces.push(format!("@{sid}"));
            } else {
                pieces.push(piece.to_string());
            }
        }
        out.push(pieces.join(","));
    }
    let mut line = out.join(" ");
    line.push('\n');
    Some(line)
}

/// Whether the subscription's target set is the live `@*` roster, which never
/// ends by its sessions exiting.
fn targets_everything(request: &str) -> bool {
    request.split_whitespace().any(|t| t == "@*")
}

/// Follows the push stream's FRAMING just far enough to know when every
/// subscribed session has said `EVENT <local> exited` — the stream's own
/// end-of-session marker. Header lines are parsed; the bodies that follow some
/// of them (a `screen` DELTA's rows, a `cells` DELTA's bytes, a `BYTES` frame)
/// are skipped by count, so a body that happens to read `EVENT 1 exited` is
/// never mistaken for a frame. Anything it cannot frame makes it `lost`, which
/// only ever means "cannot say the session is over".
#[derive(Debug, Default)]
pub(crate) struct FrameTracker {
    line: Vec<u8>,
    skip_lines: u64,
    skip_bytes: u64,
    /// `(local, sid, exited)` per `sub` line seen on the current leg.
    targets: Vec<(String, String, bool)>,
    everything: bool,
    lost: bool,
}

/// The longest header line the tracker keeps; a longer one is not a frame
/// header this client knows.
const MAX_HEADER: usize = 4096;

impl FrameTracker {
    /// A tracker for the stream `request` opened.
    pub(crate) fn new(request: &str) -> Self {
        Self {
            everything: targets_everything(request),
            ..Self::default()
        }
    }

    /// A redialled leg starts a new stream: new `sub` lines, new locals.
    pub(crate) fn new_leg(&mut self) {
        let everything = self.everything;
        *self = Self {
            everything,
            ..Self::default()
        };
    }

    /// Whether the stream said every one of its sessions exited.
    pub(crate) fn session_over(&self) -> bool {
        !self.lost
            && !self.everything
            && !self.targets.is_empty()
            && self.targets.iter().all(|(_, _, exited)| *exited)
    }

    /// `(local, sid)` per `sub` line of the current leg: what the server tied each
    /// local number it answered with to — how a redial names by sid a target the
    /// request named by number ([`resume_request`]).
    pub(crate) fn sids(&self) -> Vec<(String, String)> {
        self.targets
            .iter()
            .map(|(local, sid, _)| (local.clone(), sid.clone()))
            .collect()
    }

    /// Account for `bytes` relayed to stdout.
    pub(crate) fn feed(&mut self, mut bytes: &[u8]) {
        while !bytes.is_empty() && !self.lost {
            if self.skip_bytes > 0 {
                let n = usize::try_from(self.skip_bytes)
                    .unwrap_or(usize::MAX)
                    .min(bytes.len());
                self.skip_bytes -= n as u64;
                bytes = &bytes[n..];
                continue;
            }
            let Some(nl) = bytes.iter().position(|&b| b == b'\n') else {
                self.line.extend_from_slice(bytes);
                if self.line.len() > MAX_HEADER {
                    self.lost = true;
                }
                return;
            };
            self.line.extend_from_slice(&bytes[..nl]);
            bytes = &bytes[nl + 1..];
            let line = std::mem::take(&mut self.line);
            if self.skip_lines > 0 {
                self.skip_lines -= 1;
            } else {
                self.header(&String::from_utf8_lossy(&line));
            }
        }
    }

    fn header(&mut self, line: &str) {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        match tokens.as_slice() {
            ["sub", local, sid, ..] => {
                self.targets
                    .push(((*local).to_string(), (*sid).to_string(), false));
            }
            ["EVENT", local, "exited", ..] => {
                for (known, _, exited) in &mut self.targets {
                    if known == local {
                        *exited = true;
                    }
                }
            }
            ["DELTA", _, _, "screen", rows, ..] => match rows.parse::<u64>() {
                Ok(rows) => self.skip_lines = rows,
                Err(_) => self.lost = true,
            },
            ["DELTA", _, _, "cells", len, ..] | ["BYTES", _, len, ..] => match len.parse::<u64>() {
                // The body, then its trailing newline.
                Ok(len) => self.skip_bytes = len.saturating_add(1),
                Err(_) => self.lost = true,
            },
            _ => {}
        }
    }
}

/// How one leg of a relayed stream ended.
enum Leg {
    /// The server hung up (EOF, reset).
    HungUp,
    /// The explicit `--timeout` wall clock ran out.
    Timeout,
}

/// Relay one connection's frames to `out` until the server hangs up or the
/// `watch` wall clock (an explicit `--timeout`) runs out. Every frame is flushed
/// as it arrives, so a pipeline sees it before the next read blocks.
fn pump(
    stream: &CtlStream,
    reader: &mut BufReader<&CtlStream>,
    out: &mut impl io::Write,
    tracker: &mut FrameTracker,
    watch: Option<(Instant, Duration)>,
) -> io::Result<Leg> {
    let poll = Duration::from_millis(250);
    // Ignored on failure: macOS answers `setsockopt` on a socket whose peer has
    // already gone with EINVAL (measured 2026-09-24), and such a socket's next
    // read returns the buffered frames and then EOF without waiting anyway.
    let _ = stream.set_read_timeout(watch.map(|_| poll));
    let mut buf = [0u8; 8192];
    loop {
        if let Some((start, limit)) = watch
            && start.elapsed() >= limit
        {
            out.flush()?;
            return Ok(Leg::Timeout);
        }
        match reader.read(&mut buf) {
            Ok(0) => {
                out.flush()?;
                return Ok(Leg::HungUp);
            }
            Ok(n) => {
                out.write_all(&buf[..n])?;
                out.flush()?;
                tracker.feed(&buf[..n]);
            }
            Err(ref e) if super::is_timeout_error(e) && watch.is_some() => {}
            Err(ref e) if is_hangup(e) => {
                out.flush()?;
                return Ok(Leg::HungUp);
            }
            Err(e) => return Err(e),
        }
    }
}

/// The first leg of a `subscribe` relay, already past its `OK subscribe` ack.
pub(crate) struct FirstLeg<'a, 'b> {
    pub(crate) stream: &'b CtlStream,
    pub(crate) reader: &'a mut BufReader<&'b CtlStream>,
    /// The path it was dialled at, resolved.
    pub(crate) dialed: &'a str,
    /// The server's pid, when the platform can say.
    pub(crate) server: Option<u32>,
}

/// Relay a `subscribe` push stream to `out` across server replacements (the
/// rule on this module). `watch` is the explicit `--timeout` wall clock, over
/// the WHOLE relay, redials included.
pub(crate) fn relay_subscription(
    first: FirstLeg<'_, '_>,
    request: &str,
    watch: Option<Duration>,
    redial: &Redial,
    out: &mut impl io::Write,
) -> io::Result<ExitCode> {
    let watch = watch.map(|limit| (Instant::now(), limit));
    // The wall clock the successor wait may not outlast (the rule on this
    // module): a hang-up is inside the watch, never added to it.
    let until = watch.map(|(start, limit)| start + limit);
    let mut tracker = FrameTracker::new(request);
    if let Leg::Timeout = pump(first.stream, first.reader, out, &mut tracker, watch)? {
        return Ok(ExitCode::from(super::EXIT_TIMEOUT));
    }
    // What the next leg asks for: the original, then each resumed form — whose
    // local targets are already sids, so a later leg's new numbers never matter.
    let mut request = request.to_string();
    let mut dialed = first.dialed.to_string();
    let mut server = first.server;
    loop {
        if tracker.session_over() {
            return Ok(ExitCode::SUCCESS);
        }
        let (path, pid) = match redial.after_hangup(&dialed, server, until) {
            // Today's behaviour, where it is still the truth or all there is.
            Hangup::StillServing | Hangup::Unknown => return Ok(ExitCode::SUCCESS),
            Hangup::Gone {
                waited,
                by_deadline,
            } => {
                super::stderr_line(&gone_note(server, waited, by_deadline))?;
                return Ok(ExitCode::from(EXIT_REPLACED));
            }
            Hangup::Successor { path, pid } => (path, pid),
        };
        let Ok(stream) = CtlStream::connect(aterm_uds::latest::resolve(&path)) else {
            // It answered a moment ago; look again from here.
            dialed = path;
            continue;
        };
        // Bounded handshake — by the watch's remainder too; best effort for the
        // same EINVAL reason as `pump`.
        let handshake = until.map_or(SUCCESSOR_BOUND, |until| {
            until
                .saturating_duration_since(Instant::now())
                .clamp(PROBE_FLOOR, SUCCESSOR_BOUND)
        });
        let _ = stream.set_write_timeout(Some(handshake));
        let _ = stream.set_read_timeout(Some(handshake));
        let Some(resumed) = resume_request(&request, &tracker.sids()) else {
            super::stderr_line(&format!(
                "aterm was replaced (pid {} -> {pid}), and this subscription names a session by \
                 its local number (`@<n>`), which the successor numbers afresh and the stream \
                 never tied to a session id; re-issue it by sid",
                server.unwrap_or(0)
            ))?;
            return Ok(ExitCode::from(EXIT_REPLACED));
        };
        if super::send_request(&stream, super::read_token_for(&path).as_deref(), &resumed).is_err()
        {
            dialed = path;
            server = Some(pid);
            continue;
        }
        let mut reader = BufReader::new(&stream);
        let mut status = String::new();
        let got = super::read_bounded_line(&mut reader, &mut status);
        let status = status.trim_end_matches(['\r', '\n']);
        if !matches!(got, Ok(n) if n > 0) || !status.starts_with("OK") {
            if !status.is_empty() {
                super::stderr_line(status)?;
            }
            super::stderr_line(&format!(
                "aterm was replaced (pid {} -> {pid}), and its successor did not resume this \
                 subscription; re-issue it",
                server.unwrap_or(0)
            ))?;
            return Ok(ExitCode::from(EXIT_REPLACED));
        }
        super::stderr_line(status)?;
        super::stderr_line(&format!(
            "aterm was replaced (pid {} -> {pid}, an update); the stream resumed on {path} — \
             frames between the two were not delivered, and each target starts over with its \
             current state",
            server.unwrap_or(0)
        ))?;
        request = resumed;
        tracker.new_leg();
        if let Leg::Timeout = pump(&stream, &mut reader, out, &mut tracker, watch)? {
            return Ok(ExitCode::from(super::EXIT_TIMEOUT));
        }
        dialed = path;
        server = Some(pid);
    }
}

/// The note for a hang-up nothing replaced ([`Hangup::Gone`]'s fields).
pub(crate) fn gone_note(server: Option<u32>, waited: Duration, by_deadline: bool) -> String {
    let pid = server.unwrap_or(0);
    let wait = if waited.is_zero() {
        "and this call follows it no further (a `--pid` pin names one process, and a blocking \
         read is asked again only once)"
            .to_string()
    } else if by_deadline {
        format!(
            "and no instance replaced it within the {:.1} s its --timeout left",
            waited.as_secs_f64()
        )
    } else {
        format!(
            "and no instance replaced it within {:.1} s",
            waited.as_secs_f64()
        )
    };
    format!(
        "aterm (pid {pid}) went away {wait} — an update that did not come back, a crash or a \
         quit; the exchange cannot be resumed (exit {EXIT_REPLACED})"
    )
}

#[cfg(test)]
#[path = "redial_tests.rs"]
mod tests;
