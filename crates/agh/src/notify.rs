// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Fabric posts (design §21.5): one broadcast through the session's own control socket after
//! an event is observed or a write settles `Done`, opt-in (`--notify`, `How::notify(true)`),
//! and silent when it fails.

#[cfg(unix)]
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::Mutex;
#[cfg(any(unix, test, debug_assertions))]
use std::sync::PoisonError;
use std::time::Duration;

use crate::client::Github;
use crate::fx::Fx;
use crate::github::How;
use crate::identity::EnvSource;
use crate::journal::IntentId;
use crate::redact::Redactor;
use crate::slug::Slug;

/// The broker's inline limit on a post's body.
const BODY_MAX: usize = 4096;

/// The longest reply line read from the broker.
#[cfg(unix)]
const LINE_MAX: usize = 1 << 20;

/// The longest `ERR` word or fabric state a receipt carries.
#[cfg(unix)]
const WORD_MAX: usize = 32;

/// The two topics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Topic {
    /// `agh.release`: a wait's outcome.
    Release,
    /// `agh.write`: a settled write.
    Write,
}

impl Topic {
    /// `agh.release`, `agh.write`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Topic::Release => "agh.release",
            Topic::Write => "agh.write",
        }
    }
}

/// One post.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Notice {
    /// The topic.
    pub topic: Topic,
    /// The repository.
    pub repo: Slug,
    /// The tag.
    pub tag: String,
    /// On `agh.release`, the held conditions joined by `+`, each condition's slug passed
    /// through `Redactor::shapes`, then written `aterm_codec::pct::encode`d with each `+` then
    /// written `%2B`; on `agh.write`, `created`, `uploaded`, `published` or `deleted`, passed
    /// through `Redactor::shapes` and `pct::encode` as well, so no constructor leaves a blank,
    /// CR or LF in it. A post whose line would hold a CR or LF is never sent
    /// (`Failed("framing")`).
    pub event: String,
    /// The asset count, when the event has one.
    pub n: Option<u32>,
    /// The release id, when known.
    pub id: Option<u64>,
    /// The broker's dedup key: a post whose key breaks the broker's rule (1–64 bytes of
    /// `[A-Za-z0-9._:-]`) is never sent (`Failed("key")`).
    pub key: String,
}

/// `agh-` and the first 16 lowercase hex of the SHA-256 of `parts` joined by `0x00`.
fn hashed_key(parts: &[&str]) -> String {
    let mut hasher = aterm_digest::Sha256::new();
    for (at, part) in parts.iter().enumerate() {
        if at > 0 {
            hasher.update(b"\x00");
        }
        hasher.update(part.as_bytes());
    }
    let digest = hasher.finalize();
    let mut out = String::with_capacity(20);
    out.push_str("agh-");
    aterm_codec::hex::push_lower(&mut out, &digest[..8]);
    out
}

/// A number's decimal, or nothing.
fn decimal<T: std::fmt::Display>(value: Option<T>) -> String {
    value.map(|v| v.to_string()).unwrap_or_default()
}

/// One held condition's slug for a wait's event: `pct::encode`d, each `+` then `%2B`. The
/// caller redacts it first, because encoding writes a blank `%20`, which the redactor's
/// `bearer` and `basic` rules no longer see as a blank.
fn encode_condition(slug: &str) -> String {
    let encoded = aterm_codec::pct::encode(slug);
    if encoded.contains('+') {
        encoded.replace('+', "%2B")
    } else {
        encoded
    }
}

/// A write's event as the body writes it: redacted, then `pct::encode`d, so it holds no
/// blank, CR or LF (each of the four words is written as it is).
fn encode_event(event: &str) -> String {
    aterm_codec::pct::encode(&Redactor::shapes(event))
}

/// The tag as the body writes it: as it is, unless it holds a byte that is not ASCII
/// graphic, `=` or `%`; then `pct::encode`d with each `=` then `%3D`.
fn encode_tag(tag: &str) -> String {
    let plain = tag
        .bytes()
        .all(|b| b.is_ascii_graphic() && b != b'=' && b != b'%');
    if plain {
        tag.to_string()
    } else {
        aterm_codec::pct::encode(tag).replace('=', "%3D")
    }
}

impl Notice {
    /// A wait's notice: key `agh-` and the first 16 hex of
    /// `sha256("agh.release" ‖ 0x00 ‖ repo ‖ 0x00 ‖ tag ‖ 0x00 ‖ event ‖ 0x00 ‖ n or "" ‖ 0x00 ‖
    /// id or "")`, the repo lowercased and `event` the encoded join of the slugs as given, so a
    /// release cut again under the same tag (a new id) is a new post. The topic leads the hash
    /// because the bridge pins a key per session whatever its topic: a wait held on
    /// `published` alone would otherwise hash the bytes of the same release's `converged`
    /// publish. No `converged` or `converged_for` input can begin `agh.release` and `0x00`,
    /// because their first part is a lowercased slug, which always holds a `/`. The `event`
    /// field joins the slugs each redacted before it is encoded, so the key is the same whether
    /// or not a slug is.
    #[must_use]
    pub fn wait(repo: &Slug, tag: &str, held: &[&str], n: Option<u32>, id: Option<u64>) -> Notice {
        let join = |redact: bool| {
            let mut out = String::new();
            for (i, slug) in held.iter().enumerate() {
                if i > 0 {
                    out.push('+');
                }
                if redact {
                    out.push_str(&encode_condition(&Redactor::shapes(slug)));
                } else {
                    out.push_str(&encode_condition(slug));
                }
            }
            out
        };
        let key = hashed_key(&[
            Topic::Release.as_str(),
            &repo.key(),
            tag,
            &join(false),
            &decimal(n),
            &decimal(id),
        ]);
        let event = join(true);
        Notice {
            topic: Topic::Release,
            repo: repo.clone(),
            tag: tag.to_string(),
            event,
            n,
            id,
            key,
        }
    }

    /// A one-shot write's notice: key `agh-{intent}`.
    #[must_use]
    pub fn intent(
        repo: &Slug,
        tag: &str,
        event: &str,
        intent: &IntentId,
        id: Option<u64>,
    ) -> Notice {
        Notice {
            topic: Topic::Write,
            repo: repo.clone(),
            tag: tag.to_string(),
            event: encode_event(event),
            n: None,
            id,
            key: format!("agh-{intent}"),
        }
    }

    /// A Converging write's notice, which has no intent: key `agh-` and the first 16 hex of
    /// `sha256(repo ‖ 0x00 ‖ tag ‖ 0x00 ‖ event ‖ 0x00 ‖ n or "" ‖ 0x00 ‖ id or "")`, so two
    /// uploads to one release, whose counts differ, are two posts.
    #[must_use]
    pub fn converged(
        repo: &Slug,
        tag: &str,
        event: &str,
        n: Option<u32>,
        id: Option<u64>,
    ) -> Notice {
        let key = hashed_key(&[&repo.key(), tag, event, &decimal(n), &decimal(id)]);
        Notice::write_keyed(repo, tag, event, n, id, key)
    }

    /// A Converging write's notice under `key`, which its constructor hashed.
    fn write_keyed(
        repo: &Slug,
        tag: &str,
        event: &str,
        n: Option<u32>,
        id: Option<u64>,
        key: String,
    ) -> Notice {
        Notice {
            topic: Topic::Write,
            repo: repo.clone(),
            tag: tag.to_string(),
            event: encode_event(event),
            n,
            id,
            key,
        }
    }

    /// A Converging write's notice that names the asset it wrote or removed: as `converged`,
    /// with `‖ 0x00 ‖ object` (its decimal) appended to the key. The key then names the event,
    /// not the state after it: a clobber that keeps the count (its rename leaves a new asset
    /// id), or a delete that returns to an earlier count, is a new post, while a rerun of a
    /// lost upload, which finds the same asset, keeps its key.
    #[must_use]
    pub fn converged_for(
        repo: &Slug,
        tag: &str,
        event: &str,
        n: Option<u32>,
        id: Option<u64>,
        object: u64,
    ) -> Notice {
        let object = object.to_string();
        let key = hashed_key(&[&repo.key(), tag, event, &decimal(n), &decimal(id), &object]);
        Notice::write_keyed(repo, tag, event, n, id, key)
    }

    /// `v=1 repo={owner}/{repo} tag={tag} event={event}`, then ` n={n}` and ` id={id}` when
    /// set. A tag holding a byte that is not ASCII graphic (a space, a control byte, any
    /// non-ASCII byte), `=` or `%` is written `aterm_codec::pct::encode`d, with each `=` then
    /// written `%3D` (`pct::encode` keeps `=`), so the body stays one line of `key=value` words.
    /// The tag is caller text, so it passes `Redactor::shapes` before it is encoded (encoding
    /// writes a blank `%20`, which hides a `bearer {token}` from the redactor), and each
    /// `key=value` word passes it again on its own before the words are joined: a token-shaped
    /// tag is written `[redacted]`, which keeps the body one line, and no rule reads across
    /// the blank between two words (a tag ending `bearer` keeps the `event=` word after it).
    #[must_use]
    pub fn body(&self) -> String {
        let mut out = String::new();
        let mut push_word = |word: &str| {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(&Redactor::shapes(word));
        };
        push_word("v=1");
        push_word(&format!("repo={}", self.repo));
        push_word(&format!("tag={}", encode_tag(&Redactor::shapes(&self.tag))));
        push_word(&format!("event={}", self.event));
        if let Some(n) = self.n {
            push_word(&format!("n={n}"));
        }
        if let Some(id) = self.id {
            push_word(&format!("id={id}"));
        }
        out
    }

    /// `@{sid} post to=say:{topic} kind=note key={key} -- {body}`, built from `body()`, so it
    /// is redacted as `body()` is.
    #[must_use]
    pub fn line(&self, sid: &str) -> String {
        self.line_with(sid, &self.body())
    }

    /// `line(sid)` around `body`, which is `self.body()`.
    fn line_with(&self, sid: &str, body: &str) -> String {
        format!(
            "@{sid} post to=say:{} kind=note key={} -- {body}",
            self.topic.as_str(),
            self.key,
        )
    }

    /// `line(sid)` when it may be sent: a key that breaks the broker's rule is
    /// `Failed("key")`, a body over the broker's 4 KiB inline limit `Failed("oversized")`, and
    /// a line holding a CR or LF `Failed("framing")`, the connection's one-line framing.
    fn sendable(&self, sid: &str) -> Result<String, Posted> {
        if !broker_word(&self.key) {
            return Err(failed("key"));
        }
        let body = self.body();
        if body.len() > BODY_MAX {
            return Err(failed("oversized"));
        }
        let line = self.line_with(sid, &body);
        if line.contains(['\r', '\n']) {
            return Err(failed("framing"));
        }
        Ok(line)
    }
}

/// How one post ended, for the receipt's `notify=` field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Posted {
    /// `notify=sent:{id}`.
    Sent(String),
    /// `notify=skipped:{why}`: no session (`no-session`), a nested multiplexer (`mux`),
    /// a fabric that is not connected (`fabric={state}`), or a tape (`taped`).
    Skipped(String),
    /// `notify=failed:{why}`: no socket, a refused AUTH, an `ERR` reply, a timeout, a line
    /// that may not be sent (`framing`, `key`, `oversized`), or `unsupported` off unix. `why`
    /// is one redacted word or short phrase.
    Failed(String),
}

impl Posted {
    /// The receipt's value: `sent:{id}`, `skipped:{why}` or `failed:{why}`.
    #[must_use]
    pub fn receipt(&self) -> String {
        match self {
            Posted::Sent(id) => format!("sent:{id}"),
            Posted::Skipped(why) => format!("skipped:{why}"),
            Posted::Failed(why) => format!("failed:{why}"),
        }
    }
}

/// `Posted::Failed(why)`.
fn failed(why: &str) -> Posted {
    Posted::Failed(why.to_string())
}

/// The broker's key and id rule: 1–64 bytes of `[A-Za-z0-9._:-]`.
fn broker_word(text: &str) -> bool {
    (1..=64).contains(&text.len())
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

/// A reply word as a receipt carries it: lowercased, only `[a-z0-9=_-]`, at most 32 bytes.
#[cfg(unix)]
fn receipt_word(word: &str) -> String {
    word.chars()
        .map(|c| c.to_ascii_lowercase())
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '=' | '_' | '-'))
        .take(WORD_MAX)
        .collect()
}

/// An `ERR` line's first word after `ERR` (`receipt_word`), or `err` when it has none.
#[cfg(unix)]
fn err_word(rest: &str) -> String {
    let word = receipt_word(rest.split_ascii_whitespace().next().unwrap_or(""));
    if word.is_empty() {
        "err".to_string()
    } else {
        word
    }
}

/// What a status reply says of the fabric: `None` when it holds the word `fabric=connected`,
/// else the skip. The state is cleaned as an `ERR` word is (`receipt_word`); a state that
/// cleans to nothing, or to `connected` (`fabric=Connected`, `fabric=connected,`), is
/// `unknown`, so a skip never names the one state that posts.
#[cfg(unix)]
fn fabric_skip(reply: &str) -> Option<Posted> {
    let mut words = reply.split_ascii_whitespace();
    if words.clone().any(|word| word == "fabric=connected") {
        return None;
    }
    let state = words
        .find_map(|word| word.strip_prefix("fabric="))
        .map(receipt_word)
        .filter(|state| !state.is_empty() && state != "connected")
        .unwrap_or_else(|| "unknown".to_string());
    Some(Posted::Skipped(format!("fabric={state}")))
}

/// What the post's reply says: `OK {id}` is `Sent(id)`, `ERR …` is `Failed(word)`, and
/// anything else `Failed("reply")`.
#[cfg(unix)]
fn post_reply(reply: &str) -> Posted {
    if let Some(rest) = reply.strip_prefix("ERR") {
        return Posted::Failed(err_word(rest));
    }
    match reply.strip_prefix("OK ") {
        Some(id) if broker_word(id) => Posted::Sent(id.to_string()),
        _ => failed("reply"),
    }
}

/// An I/O error as the receipt names it: a read or write past its bound is `timeout`, any
/// other `closed`.
#[cfg(unix)]
fn io_failure(error: &std::io::Error) -> Posted {
    match error.kind() {
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => failed("timeout"),
        _ => failed("closed"),
    }
}

/// One reply line of at most 1 MiB, ending at the first `\n`, which is not returned.
#[cfg(unix)]
fn read_line(reader: &mut impl BufRead) -> Result<String, Posted> {
    let mut buf = Vec::new();
    let cap = u64::try_from(LINE_MAX + 1).unwrap_or(u64::MAX);
    reader
        .take(cap)
        .read_until(b'\n', &mut buf)
        .map_err(|e| io_failure(&e))?;
    if buf.last() != Some(&b'\n') {
        return Err(failed(if buf.len() > LINE_MAX {
            "oversized"
        } else {
            "closed"
        }));
    }
    buf.pop();
    if buf.contains(&b'\r') {
        return Err(failed("framing"));
    }
    String::from_utf8(buf).map_err(|_| failed("framing"))
}

/// Write `line` and its `\n`; a line holding a CR or LF is `Failed("framing")`, unsent, as
/// aterm-agent's `RelayClient` refuses one.
#[cfg(unix)]
fn write_line(mut stream: &aterm_uds::CtlStream, line: &str) -> Result<(), Posted> {
    if line.contains(['\r', '\n']) {
        return Err(failed("framing"));
    }
    let mut bytes = Vec::with_capacity(line.len() + 1);
    bytes.extend_from_slice(line.as_bytes());
    bytes.push(b'\n');
    stream.write_all(&bytes).map_err(|e| io_failure(&e))
}

mod sealed {
    /// Only agh's notifiers.
    pub trait Sealed {}
}

/// Where posts go. Sealed: `SessionNotifier`, `NoNotifier` and the tests' `RecordingNotifier`
/// are the only ones, so no caller can route a post elsewhere.
pub trait Notifier: sealed::Sealed + Send + Sync {
    /// Post `notice` once, through `fx` (`Fx::emit_fabric`), and say how it ended. Never
    /// an error: a failed post changes no exit code (design §21.5).
    fn post(&self, fx: &Fx, notice: &Notice) -> Posted;
}

/// Posts nothing: `Posted::Skipped("off")`.
pub struct NoNotifier;

impl sealed::Sealed for NoNotifier {}

impl Notifier for NoNotifier {
    fn post(&self, _fx: &Fx, _notice: &Notice) -> Posted {
        Posted::Skipped("off".to_string())
    }
}

/// The bound on each phase of a post: 10 s.
const PHASE_BOUND: Duration = Duration::from_secs(10);

/// The session's own socket (design §21.5): the six steps of `SessionNotifier::post`.
pub struct SessionNotifier {
    /// The session, `s-<hex>`; else the skip every post answers (`no-session`, `mux`).
    sid: Result<String, &'static str>,
    /// The socket, when a test pinned it; else resolved per post.
    sock: Option<std::path::PathBuf>,
    /// The bound on each phase.
    bound: Duration,
    /// The first post that met an instance it could not reach or that did not answer
    /// (`no-socket`, `no-token`, `closed`, `timeout`): every later post of this notifier
    /// answers it at once, so one run pays the bound at most once.
    tripped: Mutex<Option<Posted>>,
}

/// Whether `posted` names an instance that later posts of the run would meet again.
#[cfg(unix)]
fn trips(posted: &Posted) -> bool {
    matches!(posted, Posted::Failed(why)
        if matches!(why.as_str(), "no-socket" | "no-token" | "closed" | "timeout"))
}

/// The `ATERM_MUX` values that turn the multiplexer check off, as aterm-ctl reads them.
fn mux_check_off(value: &str) -> bool {
    matches!(value, "0" | "off" | "no" | "none" | "false")
}

/// `tmux` or `screen` when `value`'s first `-`/`.` part names one (a `TERM` or marker).
fn mux_named(value: &str) -> bool {
    matches!(
        value.split(['-', '.']).next().unwrap_or(""),
        "tmux" | "screen"
    )
}

/// Whether `env` is a shell inside a multiplexer nested in an aterm session, as aterm-ctl's
/// `detect_mux_nesting` decides it (so a post skips where `aterm ctl @self …` refuses). A value
/// that is not UTF-8 counts as unset, as aterm-ctl's `env::var(..).ok()` reads it:
/// - `ATERM_MUX` non-empty: nested, unless it is `0`, `off`, `no`, `none` or `false`;
/// - else `ATERM_MUX_BASE` non-empty (aterm sets it in every session, `$TMUX|$STY` as the
///   session began, `|` outside any multiplexer): nested only when it differs from
///   `$TMUX|$STY` now and `TMUX` or `STY` is set;
/// - else, with an aterm session in view (`ATERM_MUX_OUTER_SESSION_ID` or
///   `ATERM_PARENT_SESSION_ID`), a `TERM` of `tmux…` or `screen…`, or `TMUX` set with
///   `TERM_PROGRAM=tmux`.
fn mux_nested(env: &dyn EnvSource) -> bool {
    let var = |name: &str| {
        env.var_os(name)
            .and_then(|value| value.into_string().ok())
            .filter(|value| !value.is_empty())
    };
    if let Some(marker) = var("ATERM_MUX") {
        return !mux_check_off(&marker);
    }
    let (tmux, sty) = (var("TMUX"), var("STY"));
    if let Some(base) = var("ATERM_MUX_BASE") {
        let now = format!(
            "{}|{}",
            tmux.as_deref().unwrap_or(""),
            sty.as_deref().unwrap_or("")
        );
        return base != now && (tmux.is_some() || sty.is_some());
    }
    if var("ATERM_MUX_OUTER_SESSION_ID")
        .or_else(|| var("ATERM_PARENT_SESSION_ID"))
        .is_none()
    {
        return false;
    }
    var("TERM").is_some_and(|term| mux_named(&term))
        || (tmux.is_some() && var("TERM_PROGRAM").as_deref() == Some("tmux"))
}

/// `s-` and 1–64 lowercase hex: the shape of an aterm session id.
fn session_id(text: &str) -> bool {
    text.strip_prefix("s-").is_some_and(|hex| {
        (1..=64).contains(&hex.len()) && aterm_codec::hex::is_lower(hex, hex.len())
    })
}

impl SessionNotifier {
    /// The notifier of the session `env` names: `ATERM_PARENT_SESSION_ID` of shape
    /// `s-<hex>`, unless the shell is inside a multiplexer nested in that session (aterm-ctl's
    /// rule, `mux_nested`: `Skipped("mux")`, as `aterm ctl @self post` refuses there). No
    /// session: `Skipped("no-session")` on every post.
    #[must_use]
    pub fn from_env(env: &dyn EnvSource) -> SessionNotifier {
        let sid = if mux_nested(env) {
            Err("mux")
        } else {
            env.var_os("ATERM_PARENT_SESSION_ID")
                .and_then(|value| value.into_string().ok())
                .filter(|sid| session_id(sid))
                .ok_or("no-session")
        };
        SessionNotifier {
            sid,
            sock: None,
            bound: PHASE_BOUND,
            tripped: Mutex::new(None),
        }
    }

    /// Tests: the notifier of session `sid` on the socket at `sock`, no resolution.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn at_socket(sock: &std::path::Path, sid: &str) -> SessionNotifier {
        SessionNotifier {
            sid: Ok(sid.to_string()),
            sock: Some(sock.to_path_buf()),
            bound: PHASE_BOUND,
            tripped: Mutex::new(None),
        }
    }

    /// Tests: the same notifier, its session (or skip) still `from_env`'s, on the socket at
    /// `sock`, so a test of an env's session reaches a scratch broker and never resolves a
    /// running aterm's socket.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn with_socket_for_tests(mut self, sock: &std::path::Path) -> SessionNotifier {
        self.sock = Some(sock.to_path_buf());
        self
    }

    /// Tests: the same notifier with `bound` in place of the 10 s bound per phase, so a test
    /// of a server that never answers ends in a second.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn with_bound_for_tests(mut self, bound: Duration) -> SessionNotifier {
        self.bound = bound;
        self
    }

    /// The six steps over the socket, each read and write bounded by `self.bound`; once a
    /// post has tripped (`tripped`), its answer, with nothing connected. Steps 1 and 2 connect
    /// through aterm-ctl with a blocking connect, which macOS refuses at once on a full accept
    /// queue but Linux waits out, so on Linux they are not bounded until aterm-ctl offers a
    /// bounded connect.
    #[cfg(unix)]
    fn send(&self, sid: &str, line: &str) -> Posted {
        let mut tripped = self.tripped.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(posted) = tripped.as_ref() {
            return posted.clone();
        }
        let posted = match self.exchange(sid, line) {
            Ok(posted) | Err(posted) => posted,
        };
        if trips(&posted) {
            *tripped = Some(posted.clone());
        }
        posted
    }

    #[cfg(unix)]
    fn exchange(&self, sid: &str, line: &str) -> Result<Posted, Posted> {
        // 1. The socket.
        let sock = match &self.sock {
            Some(path) => path
                .to_str()
                .map(str::to_string)
                .ok_or_else(|| failed("no-socket"))?,
            None => aterm_ctl::resolve_sock_for(Some(sid)).map_err(|_| failed("no-socket"))?,
        };
        // 2. Connect, then the token beside the socket.
        let (stream, token) =
            aterm_ctl::connect_then_read_token(&sock).map_err(|_| failed("no-socket"))?;
        let token = token.map_err(|_| failed("no-token"))?;
        stream
            .set_read_timeout(Some(self.bound))
            .and_then(|()| stream.set_write_timeout(Some(self.bound)))
            .map_err(|_| failed("closed"))?;
        let mut reader = BufReader::new(&stream);
        // 3. AUTH: the broker answers nothing to a good one.
        write_line(&stream, &format!("AUTH {token}"))?;
        drop(token);
        // 4. Status: post only to a connected fabric.
        write_line(&stream, &format!("@{sid} status"))?;
        let status = read_line(&mut reader)?;
        if let Some(rest) = status.strip_prefix("ERR") {
            return Err(Posted::Failed(err_word(rest)));
        }
        if let Some(skip) = fabric_skip(&status) {
            return Err(skip);
        }
        // 5. Post.
        write_line(&stream, line)?;
        // 6. Reply.
        Ok(post_reply(&read_line(&mut reader)?))
    }
}

impl sealed::Sealed for SessionNotifier {}

impl Notifier for SessionNotifier {
    fn post(&self, fx: &Fx, notice: &Notice) -> Posted {
        let sid = match &self.sid {
            Ok(sid) => sid,
            Err(why) => return Posted::Skipped((*why).to_string()),
        };
        #[cfg(not(unix))]
        {
            let _ = (fx, notice, sid, &self.sock, self.bound, &self.tripped);
            failed("unsupported")
        }
        #[cfg(unix)]
        {
            let line = match notice.sendable(sid) {
                Ok(line) => line,
                Err(refused) => return refused,
            };
            fx.emit_fabric(&line, &mut || self.send(sid, &line))
                .unwrap_or_else(|_| failed("replay"))
        }
    }
}

/// Tests: keeps every notice and posts nothing; `Posted::Sent("recorded")`.
#[cfg(any(test, debug_assertions))]
#[derive(Debug, Default)]
pub struct RecordingNotifier {
    /// Every notice so far, in order.
    posted: Mutex<Vec<Notice>>,
}

#[cfg(any(test, debug_assertions))]
impl RecordingNotifier {
    /// Empty: `RecordingNotifier::default()`.
    #[must_use]
    pub fn new() -> RecordingNotifier {
        RecordingNotifier::default()
    }

    /// Every notice so far, in order.
    #[must_use]
    pub fn posted(&self) -> Vec<Notice> {
        self.posted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

#[cfg(any(test, debug_assertions))]
impl sealed::Sealed for RecordingNotifier {}

#[cfg(any(test, debug_assertions))]
impl Notifier for RecordingNotifier {
    fn post(&self, _fx: &Fx, notice: &Notice) -> Posted {
        self.posted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(notice.clone());
        Posted::Sent("recorded".to_string())
    }
}

/// Post `notice` through `gh.notifier()` when `how.notify`, and record its
/// `Posted::receipt` with `Github::note_notify`, so the verb's receipt prints
/// `notify=sent:…`, `notify=skipped:…` or `notify=failed:…`. When `how.notify` is false it
/// posts nothing and notes nothing. Under any tape it asks no notifier: it emits
/// `notice.line("self")` at the fabric door itself (`Fx::emit_fabric`, which answers
/// `Skipped("taped")`), so a recording and its replay emit the same bytes whatever notifier
/// each run holds; a line no post may send (a bad key, a body over 4 KiB, a CR or LF) is
/// noted as `Failed("key")`, `Failed("oversized")` or `Failed("framing")` and emitted
/// nowhere, taped or not. A resource write method calls it once, after its write is `Done`;
/// `release wait --notify` calls it with the wait's notice. Never an error.
pub(crate) fn settled(gh: &Github, how: &How, notice: &Notice) {
    if !how.notify {
        return;
    }
    let fx = gh.fx();
    let taped = fx.mode().is_taped();
    let posted = if taped {
        match notice.sendable("self") {
            Ok(line) => fx
                .emit_fabric(&line, &mut || Posted::Skipped("taped".to_string()))
                .unwrap_or_else(|_| failed("replay")),
            Err(refused) => refused,
        }
    } else {
        gh.notifier().post(fx, notice)
    };
    gh.note_notify(posted.receipt());
}

#[cfg(test)]
mod tests {
    use super::{Notice, Posted, Slug};

    /// Under a tape, `settled` emits only a line `sendable` answers `Ok`, so a body over the
    /// broker's 4 KiB limit is emitted nowhere, taped or not.
    #[test]
    fn an_oversized_body_is_never_sendable() {
        let repo = Slug::parse("fixture-dev/app").expect("a slug");
        let big = Notice::converged(&repo, &"t".repeat(4097), "uploaded", Some(1), None);
        assert_eq!(
            big.sendable("self"),
            Err(Posted::Failed("oversized".to_string()))
        );
        let small = Notice::converged(&repo, "v1", "uploaded", Some(1), None);
        assert_eq!(small.sendable("self"), Ok(small.line("self")));
    }

    /// A tab's env whose `TMUX` or `ATERM_MUX` is not UTF-8. `MapEnv::with` takes only
    /// `&str`, so it cannot carry one.
    #[cfg(unix)]
    struct RawEnv {
        /// The UTF-8 variables.
        map: crate::identity::MapEnv,
        /// The one variable whose value is not UTF-8.
        raw: &'static str,
    }

    #[cfg(unix)]
    impl crate::identity::EnvSource for RawEnv {
        fn var_os(&self, name: &str) -> Option<std::ffi::OsString> {
            use std::os::unix::ffi::OsStringExt;
            if name == self.raw {
                Some(std::ffi::OsString::from_vec(b"/tmp/\xff".to_vec()))
            } else {
                self.map.var_os(name)
            }
        }
    }

    /// A value that is not UTF-8 is unset, as aterm-ctl reads it, so a tab whose `TMUX` or
    /// `ATERM_MUX` is not UTF-8 posts where `aterm ctl @self post` works.
    #[cfg(unix)]
    #[test]
    fn a_value_that_is_not_utf8_is_unset() {
        use super::{SessionNotifier, mux_nested};
        let tab = || {
            crate::identity::MapEnv::new()
                .with("ATERM_PARENT_SESSION_ID", "s-01")
                .with("ATERM_MUX_BASE", "|")
        };
        for raw in ["TMUX", "ATERM_MUX"] {
            let env = RawEnv { map: tab(), raw };
            assert!(!mux_nested(&env), "{raw}");
            assert_eq!(
                SessionNotifier::from_env(&env).sid,
                Ok("s-01".to_string()),
                "{raw}"
            );
        }
        // The control: the same tab with a UTF-8 `TMUX` is a pane nested in it.
        let pane = RawEnv {
            map: tab().with("TMUX", "/tmp/tmux-1/default,1,0"),
            raw: "UNSET",
        };
        assert!(mux_nested(&pane));
    }
}
