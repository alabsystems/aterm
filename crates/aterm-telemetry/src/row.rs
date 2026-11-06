// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ROW, SCHEMA 1: one line of `key=value` words a row, its checksum last.
//!
//! ```text
//! schema=1 at=<unix ms> kind=<kind> key=<16 hex|-> dir=<16 hex|-> label=<program|-> sid=<sid|->
//!   host=<name> facts=<16 hex> wall_ms=<fig> cpu_ms=<fig> peak_bytes=<fig> wait_ms=<fig>
//!   cache=<fig> outcome=<ok|fail|refused|unknown> stop=<exit|signal|stalled|pressure|unknown>
//!   code=<n|-> sig=<n|-> meter=<exact|sampled|stamped> sum=<8 hex>
//! ```
//!
//! A figure (`<fig>`) is a number (a word for `cache=`), `na` where the field has no meaning
//! for the kind, or `?<source>` where aterm cannot know it: the word names what would supply
//! it (`?targo` for a build's cache tier). A figure is never guessed. `sum=` is the first four
//! bytes of SHA-256 over every byte of the line before ` sum=`, so a line a crash cut short or
//! a write interleaved reads as torn and is skipped, never half-believed.
//!
//! Privacy: a row holds digests, kinds, numbers and a label, never a command line, an
//! environment value or a file's contents. [`Digest::of`] is the only way a command line or a
//! folder reaches a row, and [`Digest::key`] drops each shell assignment (`NAME=value`) before
//! it hashes, so no environment value reaches even the digest. A label is one word: a program
//! the kernel saw run, or a shell builtin or keyword from aterm's fixed list, for a command; the
//! name the kernel showed an agent call's job; the agent a spawn named; the first word of an
//! admission's description, which its claimant program wrote (`targo`, `lab`). A command whose
//! program no read confirmed has no label and no key (`key=-`): its line may be a password typed
//! at a prompt that read no echo.
//!
//! The digests are unsalted, so the same action keys the same on every host. That also means a
//! reader of a row who can guess every word of a command line can confirm the guess. With the
//! assignments gone and an unconfirmed line unkeyed, what is left to guess is a secret passed as
//! an argument (`-p<password>`). A key under a fleet-shared secret would close that, and the
//! secrets registry holds none yet. Until it does, the log is the account's own (`0700`, its
//! files `0600`).

use std::fmt::Write as _;

/// The schema every row this build writes carries (`schema=1`).
pub const SCHEMA: u32 = 1;

/// The longest program name a row keeps ([`label`]).
pub const LABEL_MAX: usize = 32;

/// Eight bytes of SHA-256: an action's key, its folder, the host's facts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Digest(pub [u8; 8]);

impl Digest {
    /// The digest of `parts` under `domain`: each part length-prefixed, so no two different
    /// part lists hash the same bytes.
    #[must_use]
    pub fn of(domain: &str, parts: &[&[u8]]) -> Self {
        let mut h = aterm_digest::Sha256::new();
        h.update(b"aterm-telemetry/");
        h.update(domain.as_bytes());
        h.update([0]);
        for part in parts {
            h.update((part.len() as u64).to_le_bytes());
            h.update(part);
        }
        let full = h.finalize();
        let mut out = [0u8; 8];
        out.copy_from_slice(&full[..8]);
        Self(out)
    }

    /// The key of an action of `kind`: what it ran (a command line, never stored) in `dir`.
    /// `what` is hashed as its shell words with every assignment (`NAME=value`, quoted or not)
    /// dropped and the rest joined by one space ([`key_words`]), so a variable's value never
    /// reaches the digest and the same command keys the same whatever it was run with.
    #[must_use]
    pub fn key(kind: &Kind, what: &str, dir: Option<&str>) -> Self {
        let mut words_len = 0usize;
        for_each_shell_word(what, |word| {
            if !is_assignment(word) {
                if words_len > 0 {
                    words_len += 1;
                }
                words_len += word.len();
            }
        });
        let kind_bytes = kind.word().as_bytes();
        let dir_bytes = dir.unwrap_or("").as_bytes();
        let mut h = aterm_digest::Sha256::new();
        h.update(b"aterm-telemetry/key/2\0");
        h.update((kind_bytes.len() as u64).to_le_bytes());
        h.update(kind_bytes);
        h.update((words_len as u64).to_le_bytes());
        let mut first = true;
        for_each_shell_word(what, |word| {
            if !is_assignment(word) {
                if !first {
                    h.update(b" ");
                }
                first = false;
                h.update(word.as_bytes());
            }
        });
        h.update((dir_bytes.len() as u64).to_le_bytes());
        h.update(dir_bytes);
        let full = h.finalize();
        let mut out = [0u8; 8];
        out.copy_from_slice(&full[..8]);
        Self(out)
    }

    /// The digest of a folder's path.
    #[must_use]
    pub fn dir(path: &str) -> Self {
        Self::of("dir/1", &[path.as_bytes()])
    }

    /// Sixteen lowercase hex digits.
    #[must_use]
    pub fn hex(self) -> String {
        hex(&self.0)
    }

    /// The digest sixteen hex digits spell; `None` for any other word.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        let bytes = word.as_bytes();
        if bytes.len() != 16 {
            return None;
        }
        let mut out = [0u8; 8];
        for (i, pair) in bytes.chunks(2).enumerate() {
            let hi = nibble(pair[0])?;
            let lo = nibble(pair[1])?;
            out[i] = (hi << 4) | lo;
        }
        Some(Self(out))
    }
}

/// `line`'s shell words, each assignment (`NAME=value`: a name of `[A-Za-z_][A-Za-z0-9_]*`
/// before the first `=`) dropped, the rest joined by one space. A word is split at whitespace
/// outside quotes, a backslash escaping the next character, so a quoted value with spaces is one
/// word and goes with its name.
#[must_use]
pub fn key_words(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    for_each_shell_word(line, |word| {
        if !is_assignment(word) {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(word);
        }
    });
    out
}

fn for_each_shell_word<'a>(line: &'a str, mut on_word: impl FnMut(&'a str)) {
    let mut start = None;
    let (mut single, mut double, mut escaped) = (false, false, false);
    for (i, c) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' if !single => {
                start.get_or_insert(i);
                escaped = true;
            }
            '\'' if !double => {
                start.get_or_insert(i);
                single = !single;
            }
            '"' if !single => {
                start.get_or_insert(i);
                double = !double;
            }
            c if c.is_whitespace() && !single && !double => {
                if let Some(s) = start.take() {
                    on_word(&line[s..i]);
                }
            }
            _ => {
                start.get_or_insert(i);
            }
        }
    }
    if let Some(s) = start {
        on_word(&line[s..]);
    }
}

fn is_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    }
}

pub(crate) fn push_hex(s: &mut String, bytes: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    push_hex(&mut s, bytes);
    s
}

/// What kind of action a row is.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    /// A command a tab's shell ran (one OSC 133 block, C to D).
    Command,
    /// An agent's tool call: a process group an agent in a tab started (the jobs engine's
    /// `agent-call` job).
    AgentCall,
    /// A worker session `spawn` started.
    Spawn,
    /// A claim on the machine's capacity (aterm-capacity's ledger).
    Admission,
    /// A kind a newer writer knows: kept and counted under its word.
    Other(String),
}

impl Kind {
    /// The wire word.
    #[must_use]
    pub fn word(&self) -> &str {
        match self {
            Self::Command => "command",
            Self::AgentCall => "agent-call",
            Self::Spawn => "spawn",
            Self::Admission => "admission",
            Self::Other(w) => w,
        }
    }

    fn parse(word: &str) -> Option<Self> {
        Some(match word {
            "command" => Self::Command,
            "agent-call" => Self::AgentCall,
            "spawn" => Self::Spawn,
            "admission" => Self::Admission,
            // The log's own lines are no action.
            "host" | "gap" => return None,
            w if is_word(w) => Self::Other(w.to_owned()),
            _ => return None,
        })
    }
}

/// A figure: known, without meaning for the kind, or unknown with the source that would
/// supply it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fig<T> {
    /// Measured.
    Known(T),
    /// The field means nothing for this kind (a spawn has no cache tier).
    Na,
    /// aterm cannot know it; the word names the source that would supply it.
    Unknown(String),
}

impl<T: Copy> Fig<T> {
    /// The value, when known.
    #[must_use]
    pub fn known(&self) -> Option<T> {
        match self {
            Self::Known(v) => Some(*v),
            _ => None,
        }
    }
}

impl<T> Fig<T> {
    /// Unknown, `source` naming what would supply it.
    #[must_use]
    pub fn unknown(source: &str) -> Self {
        Self::Unknown(word_of(source, 32))
    }
}

/// How an action ended, as aterm saw it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Outcome {
    /// It did what it was asked (exit status 0, a spawn that started).
    Ok,
    /// It ended otherwise (a non-zero status, a signal).
    Fail,
    /// It was refused before it started.
    Refused,
    /// aterm did not see how it ended.
    Unknown,
}

impl Outcome {
    /// The wire word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Fail => "fail",
            Self::Refused => "refused",
            Self::Unknown => "unknown",
        }
    }

    fn parse(word: &str) -> Option<Self> {
        Some(match word {
            "ok" => Self::Ok,
            "fail" => Self::Fail,
            "refused" => Self::Refused,
            "unknown" => Self::Unknown,
            _ => return None,
        })
    }
}

/// Why an action stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stop {
    /// It exited (`code=` its status).
    Exit,
    /// A signal ended it (`sig=` the signal).
    Signal,
    /// The jobs engine judged it stuck at its last reading before it ended.
    Stalled,
    /// Something killed it under memory pressure.
    Pressure,
    /// aterm did not see why.
    Unknown,
}

impl Stop {
    /// The wire word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Exit => "exit",
            Self::Signal => "signal",
            Self::Stalled => "stalled",
            Self::Pressure => "pressure",
            Self::Unknown => "unknown",
        }
    }

    fn parse(word: &str) -> Option<Self> {
        Some(match word {
            "exit" => Self::Exit,
            "signal" => Self::Signal,
            "stalled" => Self::Stalled,
            "pressure" => Self::Pressure,
            "unknown" => Self::Unknown,
            _ => return None,
        })
    }

    /// The stop a shell's exit status says (POSIX: a status above 128 is a signal's, which
    /// bash, zsh and fish report as 128 plus the signal): `(stop, code, sig)`.
    #[must_use]
    pub fn of_shell_status(status: Option<i32>) -> (Self, Option<i32>, Option<i32>) {
        match status {
            None => (Self::Unknown, None, None),
            Some(s) if (129..=128 + 64).contains(&s) => (Self::Signal, Some(s), Some(s - 128)),
            Some(s) => (Self::Exit, Some(s), None),
        }
    }
}

/// How a row's wall time and CPU were taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Meter {
    /// Exactly: the shell's own marks for wall time (OSC 133 C and D) and the CPU the shell
    /// and the processes it reaped spent between them (`proc_pid_rusage`).
    Exact,
    /// At the jobs engine's passes (every 2 s while a job runs): wall time to the pass that
    /// found it gone (an upper bound), CPU and peak memory to the last pass that read it
    /// (lower bounds).
    Sampled,
    /// Wall time from the clock around the action; no CPU.
    Stamped,
}

impl Meter {
    /// The wire word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Sampled => "sampled",
            Self::Stamped => "stamped",
        }
    }

    fn parse(word: &str) -> Option<Self> {
        Some(match word {
            "exact" => Self::Exact,
            "sampled" => Self::Sampled,
            "stamped" => Self::Stamped,
            _ => return None,
        })
    }
}

/// One action's row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// When it ended, unix ms.
    pub at_ms: u64,
    /// Its kind.
    pub kind: Kind,
    /// Its key ([`Digest::key`]): the same action in the same folder keys the same on every
    /// host. `None` (`key=-`) for a command whose program no read confirmed.
    pub key: Option<Digest>,
    /// Its folder's digest ([`Digest::dir`]), where known.
    pub dir: Option<Digest>,
    /// The program's name ([`label`]); `-` where unknown.
    pub label: String,
    /// The aterm session it ran in, where one.
    pub sid: Option<String>,
    /// The host's name ([`host_word`]).
    pub host: String,
    /// The host-facts digest ([`crate::facts::HostFacts::digest`]).
    pub facts: Digest,
    /// Wall time, ms.
    pub wall_ms: Fig<u64>,
    /// CPU time, ms.
    pub cpu_ms: Fig<u64>,
    /// Peak memory (physical footprint), bytes.
    pub peak_bytes: Fig<u64>,
    /// How long it waited for admission, ms.
    pub wait_ms: Fig<u64>,
    /// The cache tier that served it.
    pub cache: Fig<String>,
    /// How it ended.
    pub outcome: Outcome,
    /// Why it stopped.
    pub stop: Stop,
    /// Its exit status, where known.
    pub code: Option<i32>,
    /// The signal that ended it, where known.
    pub sig: Option<i32>,
    /// How its wall time and CPU were taken.
    pub meter: Meter,
}

/// A program's name as a row keeps it: the last path component of `program`, at most
/// [`LABEL_MAX`] bytes of `[A-Za-z0-9._+-]` (any other byte `_`); `-` for none.
#[must_use]
pub fn label(program: &str) -> String {
    let base = program.trim().rsplit('/').next().unwrap_or("");
    let word = word_of(base, LABEL_MAX);
    if word.is_empty() {
        "-".to_owned()
    } else {
        word
    }
}

/// A host's name as a row keeps it: its first DNS label, at most 63 bytes.
#[must_use]
pub fn host_word(name: &str) -> String {
    let first = name.trim().split('.').next().unwrap_or("");
    let word = word_of(first, 63);
    if word.is_empty() {
        "-".to_owned()
    } else {
        word
    }
}

/// `text` cut to `max` bytes of `[A-Za-z0-9._+-]`, each other char `_`.
pub(crate) fn word_of(text: &str, max: usize) -> String {
    let mut out = String::with_capacity(text.len().min(max));
    push_word_of(&mut out, text, max);
    out
}

pub(crate) fn push_word_of(out: &mut String, text: &str, max: usize) {
    for c in text.chars().take(max) {
        out.push(if word_char(c) { c } else { '_' });
    }
}

const fn word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-' | ',')
}

pub(crate) fn is_word(w: &str) -> bool {
    !w.is_empty() && w.len() <= 64 && w.chars().all(word_char)
}

/// `body` with its checksum and the line feed.
pub(crate) fn seal(mut line: String) -> String {
    let d = aterm_digest::Sha256::digest(line.as_bytes());
    line.reserve(14);
    line.push_str(" sum=");
    push_hex(&mut line, &d[..4]);
    line.push('\n');
    line
}

/// A line's body when its checksum holds; `None` for a torn or altered line.
pub(crate) fn unseal(line: &str) -> Option<&str> {
    let line = line.strip_suffix('\n').unwrap_or(line);
    let (body, sum) = line.rsplit_once(" sum=")?;
    let bytes = sum.as_bytes();
    if bytes.len() != 8 {
        return None;
    }
    let d = aterm_digest::Sha256::digest(body.as_bytes());
    for (i, &[hi, lo]) in bytes.as_chunks::<2>().0.iter().enumerate() {
        let hi = nibble(hi)?;
        let lo = nibble(lo)?;
        if ((hi << 4) | lo) != d[i] {
            return None;
        }
    }
    Some(body)
}

fn push_fig_word<T: std::fmt::Display>(out: &mut String, fig: &Fig<T>) {
    match fig {
        Fig::Known(v) => {
            let _ = write!(out, "{v}");
        }
        Fig::Na => out.push_str("na"),
        Fig::Unknown(source) => {
            out.push('?');
            out.push_str(source);
        }
    }
}

fn fig_parse<T: std::str::FromStr>(word: &str) -> Option<Fig<T>> {
    if word == "na" {
        return Some(Fig::Na);
    }
    if let Some(source) = word.strip_prefix('?') {
        return is_word(source).then(|| Fig::Unknown(source.to_owned()));
    }
    word.parse().ok().map(Fig::Known)
}

fn push_opt_word<T: std::fmt::Display>(out: &mut String, v: Option<T>) {
    match v {
        Some(v) => {
            let _ = write!(out, "{v}");
        }
        None => out.push('-'),
    }
}

fn push_opt_digest(out: &mut String, d: Option<Digest>) {
    match d {
        Some(d) => push_hex(out, &d.0),
        None => out.push('-'),
    }
}

impl Row {
    pub(crate) fn write_body(&self, b: &mut String) {
        let _ = write!(
            b,
            "schema={SCHEMA} at={} kind={} key=",
            self.at_ms,
            self.kind.word()
        );
        push_opt_digest(b, self.key);
        b.push_str(" dir=");
        push_opt_digest(b, self.dir);
        b.push_str(" label=");
        let base = self.label.trim().rsplit('/').next().unwrap_or("");
        if base.is_empty() {
            b.push('-');
        } else {
            push_word_of(b, base, LABEL_MAX);
        }
        b.push_str(" sid=");
        match self.sid.as_deref() {
            Some(s) if !s.is_empty() => push_word_of(b, s, 64),
            _ => b.push('-'),
        }
        b.push_str(" host=");
        let first = self.host.trim().split('.').next().unwrap_or("");
        if first.is_empty() {
            b.push('-');
        } else {
            push_word_of(b, first, 63);
        }
        b.push_str(" facts=");
        push_hex(b, &self.facts.0);
        b.push_str(" wall_ms=");
        push_fig_word(b, &self.wall_ms);
        b.push_str(" cpu_ms=");
        push_fig_word(b, &self.cpu_ms);
        b.push_str(" peak_bytes=");
        push_fig_word(b, &self.peak_bytes);
        b.push_str(" wait_ms=");
        push_fig_word(b, &self.wait_ms);
        b.push_str(" cache=");
        match &self.cache {
            Fig::Known(tier) => push_word_of(b, tier, 32),
            other => push_fig_word(b, other),
        }
        let _ = write!(
            b,
            " outcome={} stop={} code=",
            self.outcome.word(),
            self.stop.word()
        );
        push_opt_word(b, self.code);
        b.push_str(" sig=");
        push_opt_word(b, self.sig);
        b.push_str(" meter=");
        b.push_str(self.meter.word());
    }

    pub(crate) fn body(&self) -> String {
        let mut b = String::with_capacity(360);
        self.write_body(&mut b);
        b
    }

    /// The row's line, sealed and ending in a line feed.
    #[must_use]
    pub fn line(&self) -> String {
        seal(self.body())
    }
}

/// One line of the log, read back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Line {
    /// An action's row.
    Row(Row),
    /// The host's facts, which each file opens with and which a [`Row::facts`] names.
    Host(crate::facts::HostLine),
    /// Rows the writer lost (its queue was full, or a write failed) before this line.
    Gap {
        /// When the writer noticed, unix ms.
        at_ms: u64,
        /// How many rows.
        dropped: u64,
    },
}

/// The `key=value` words of a line's body, in order, into a stack buffer.
pub(crate) fn words<'a>(body: &'a str, out: &mut [(&'a str, &'a str); 24]) -> Option<usize> {
    let mut len = 0;
    for w in body.split(' ') {
        let Some(pair) = w.split_once('=') else {
            continue;
        };
        if len >= out.len() {
            return None;
        }
        out[len] = pair;
        len += 1;
    }
    Some(len)
}

pub(crate) fn find<'a>(words: &[(&'a str, &'a str)], key: &str) -> Option<&'a str> {
    words.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

/// Parse one line of the log: `None` for a torn line, a checksum that fails, another schema, or
/// a row missing a field schema 1 requires.
#[must_use]
pub fn parse_line(line: &str) -> Option<Line> {
    let body = unseal(line)?;
    let mut buf = [("", ""); 24];
    let len = words(body, &mut buf)?;
    let w = &buf[..len];
    if find(w, "schema")? != "1" {
        return None;
    }
    let at_ms: u64 = find(w, "at")?.parse().ok()?;
    match find(w, "kind")? {
        "host" => crate::facts::HostLine::parse(at_ms, w).map(Line::Host),
        "gap" => Some(Line::Gap {
            at_ms,
            dropped: find(w, "dropped")?.parse().ok()?,
        }),
        kind => parse_row(at_ms, Kind::parse(kind)?, w).map(Line::Row),
    }
}

fn parse_row(at_ms: u64, kind: Kind, w: &[(&str, &str)]) -> Option<Row> {
    let mut key_w = None;
    let mut dir_w = None;
    let mut label_w = None;
    let mut sid_w = None;
    let mut host_w = None;
    let mut facts_w = None;
    let mut wall_ms_w = None;
    let mut cpu_ms_w = None;
    let mut peak_bytes_w = None;
    let mut wait_ms_w = None;
    let mut cache_w = None;
    let mut outcome_w = None;
    let mut stop_w = None;
    let mut code_w = None;
    let mut sig_w = None;
    let mut meter_w = None;
    for &(k, v) in w {
        match k {
            "key" => {
                key_w.get_or_insert(v);
            }
            "dir" => {
                dir_w.get_or_insert(v);
            }
            "label" => {
                label_w.get_or_insert(v);
            }
            "sid" => {
                sid_w.get_or_insert(v);
            }
            "host" => {
                host_w.get_or_insert(v);
            }
            "facts" => {
                facts_w.get_or_insert(v);
            }
            "wall_ms" => {
                wall_ms_w.get_or_insert(v);
            }
            "cpu_ms" => {
                cpu_ms_w.get_or_insert(v);
            }
            "peak_bytes" => {
                peak_bytes_w.get_or_insert(v);
            }
            "wait_ms" => {
                wait_ms_w.get_or_insert(v);
            }
            "cache" => {
                cache_w.get_or_insert(v);
            }
            "outcome" => {
                outcome_w.get_or_insert(v);
            }
            "stop" => {
                stop_w.get_or_insert(v);
            }
            "code" => {
                code_w.get_or_insert(v);
            }
            "sig" => {
                sig_w.get_or_insert(v);
            }
            "meter" => {
                meter_w.get_or_insert(v);
            }
            _ => {}
        }
    }
    let opt_digest = |v: &str| -> Option<Option<Digest>> {
        if v == "-" {
            Some(None)
        } else {
            Digest::parse(v).map(Some)
        }
    };
    let opt_i32 = |v: &str| -> Option<Option<i32>> {
        if v == "-" {
            Some(None)
        } else {
            v.parse().ok().map(Some)
        }
    };
    let cache = match cache_w? {
        v if v == "na" || v.starts_with('?') => fig_parse::<String>(v)?,
        v if is_word(v) => Fig::Known(v.to_owned()),
        _ => return None,
    };
    Some(Row {
        at_ms,
        kind,
        key: opt_digest(key_w?)?,
        dir: opt_digest(dir_w?)?,
        label: label_w?.to_owned(),
        sid: match sid_w? {
            "-" => None,
            s => Some(s.to_owned()),
        },
        host: host_w?.to_owned(),
        facts: Digest::parse(facts_w?)?,
        wall_ms: fig_parse(wall_ms_w?)?,
        cpu_ms: fig_parse(cpu_ms_w?)?,
        peak_bytes: fig_parse(peak_bytes_w?)?,
        wait_ms: fig_parse(wait_ms_w?)?,
        cache,
        outcome: Outcome::parse(outcome_w?)?,
        stop: Stop::parse(stop_w?)?,
        code: opt_i32(code_w?)?,
        sig: opt_i32(sig_w?)?,
        meter: Meter::parse(meter_w?)?,
    })
}

/// A gap line: `dropped` rows lost before `at_ms`.
#[must_use]
pub fn gap_line(at_ms: u64, dropped: u64) -> String {
    seal(format!(
        "schema={SCHEMA} at={at_ms} kind=gap dropped={dropped}"
    ))
}
