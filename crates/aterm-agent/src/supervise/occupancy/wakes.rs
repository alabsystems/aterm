// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE BOOK OF DECLARED WAKES.

use std::fmt;

use aterm_vitals::sys::predicate::{AwaitSys, Metric, Parsed};
use serde::{Deserialize, Serialize};

use super::declarer::AgentId;
use super::wake_atoms::PATH_MAX;
use super::wakes_seen::{
    AtomKind, Declarer, ExpireWhy, Prio, WakeClaim, WakeDoor, WakeSeen, WakeState, WakesSeen,
    head_key, wake_episode,
};
use crate::supervise::facts::SeedOutcome;

const MINUTE_MS: i64 = 60_000;
const HOUR_MS: i64 = 60 * MINUTE_MS;
const DAY_MS: i64 = 24 * HOUR_MS;

/// The most wakes a session's book holds, in any state.
pub const MAX_WAKES: usize = 16;

/// A wake is due within this of its declaration (`by=`), at most.
pub const BY_MAX_MS: i64 = 48 * HOUR_MS;

/// …and by this without `by=`.
pub const BY_DEFAULT_MS: i64 = 24 * HOUR_MS;

/// The longest pattern (`file:PATH~/RE/`), in characters: it bounds the matcher's compile.
pub const PATTERN_MAX: usize = 256;

/// The longest label, in bytes.
pub const LABEL_MAX: usize = 64;

/// The most atoms one `when=` joins.
pub const WHEN_MAX: usize = 8;

/// The most `unless=` atoms a wake carries.
pub const UNLESS_MAX: usize = 4;

/// Unheard `await wake` duration after which a hosted session loop's door may retell the answer.
pub const UNHEARD_MS: i64 = 5 * MINUTE_MS;

/// The usage line.
pub const USAGE: &str = "ERR usage: wake label=<token> when=<term> [and <term>…] [for=<dur>] \
                         [need=<sys> [and <sys>…] [for=<dur>]] [unless=<source>]… \
                         [prio=critical] [by=+<dur>]; <term> := <source> | at:<hh>:<mm> | \
                         in:<dur> | <sys>; <source> := file:<path>[~/<re>/] | gone:<path> | \
                         pid:<n> exited; <sys> := a machine term of await sys; <path> and <re> \
                         percent-encoded (a literal ~ is %7E, a space %20); <dur> := <n>(s|m|h)\n";

/// WHY A DECLARATION IS NOT READ: each a closed sentence, so no word of the agent's is echoed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeUsage {
    /// The form is not the grammar's: [`USAGE`].
    Grammar,
    /// `label=` is not a token.
    Label,
    /// No `when=`.
    NoWhen,
    /// A control character.
    Control,
    /// A path over [`PATH_MAX`] bytes.
    PathLong,
    /// A pattern over [`PATTERN_MAX`] characters.
    PatternLong,
    /// A pattern that does not compile.
    Pattern,
    /// A percent escape that is not `%XX` of UTF-8.
    Escape,
    /// A volume term (`free`, `disk=`, `vol=`).
    Volume,
    /// A source or time atom in `need=`.
    NeedSys,
    /// A sys or time atom in `unless=`.
    UnlessSource,
    /// `for=` or `scope=` with no sys term before it.
    ForWithoutSys,
    /// `by=` or `in:` past [`BY_MAX_MS`], or `by=` without its `+`.
    By,
    /// `task:@<sid>`, which `await task` serves.
    AwaitTask,
    /// `git(…)`, `atpkg(…)` or `workflow.returned`.
    Slice3,
    /// A claim atom or `irreversible`.
    Slice4,
    /// `job=` or `task=`: a job's end is `pid:<n> exited`.
    JobTerm,
    /// `cpu.<name>`: a performance level is named by its index.
    CpuName,
    /// `timeout=`: a wake's limit is `by=`.
    Timeout,
    /// More than [`WHEN_MAX`] atoms in `when=`, or [`UNLESS_MAX`] `unless=`.
    TooMany,
}

impl WakeUsage {
    /// Its sentence.
    #[must_use]
    pub const fn words(self) -> &'static str {
        match self {
            WakeUsage::Grammar => "the form is not wake's",
            WakeUsage::Label => "label= takes 1 to 64 of A-Z a-z 0-9 . _ -",
            WakeUsage::NoWhen => "when= is required",
            WakeUsage::Control => "a control character is refused",
            WakeUsage::PathLong => "a path is at most 1024 bytes",
            WakeUsage::PatternLong => "a pattern is at most 256 characters",
            WakeUsage::Pattern => "the pattern does not compile",
            WakeUsage::Escape => "a percent escape is %XX of UTF-8",
            WakeUsage::Volume => {
                "need= takes machine terms; free, disk= and vol= arrive in a later slice"
            }
            WakeUsage::NeedSys => "need= takes machine terms alone",
            WakeUsage::UnlessSource => "unless= takes file:, gone: and pid:<n> exited alone",
            WakeUsage::ForWithoutSys => "for= and scope= follow a machine term",
            WakeUsage::By => "by= is +<dur>, at most +48h; in: at most 48h",
            WakeUsage::AwaitTask => "a task is await task's (aterm ctl await task)",
            WakeUsage::Slice3 => "git(), atpkg() and workflow.returned arrive in slice 3",
            WakeUsage::Slice4 => "claim atoms and irreversible arrive in slice 4",
            WakeUsage::JobTerm => "a job's end is pid:<n> exited",
            WakeUsage::CpuName => "a performance level is cpu.l<n>",
            WakeUsage::Timeout => "a wake's limit is by=",
            WakeUsage::TooMany => {
                "when= joins at most 8 atoms, and unless= is given at most 4 times"
            }
        }
    }

    /// The reply refusing it.
    #[must_use]
    pub fn reply(self) -> String {
        match self {
            WakeUsage::Grammar => USAGE.to_string(),
            other => format!("ERR usage wake: {}\n", other.words()),
        }
    }
}

/// A LABEL (`label=`): 1 to [`LABEL_MAX`] bytes of `A-Z a-z 0-9 . _ -`.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    /// `text` as a label, or `None`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let ok = (1..=LABEL_MAX).contains(&text.len())
            && text
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
        ok.then(|| Self(text.to_string()))
    }

    /// The label.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Token({} bytes)", self.0.len())
    }
}

impl Serialize for Token {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Token {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        checked_str(d, |text| Self::parse(&text).ok_or(WakeUsage::Label))
    }
}

/// A WAKE'S PATH, percent-decoded: under the home folder (`~/…` on the wire) or as given.
#[derive(Clone, PartialEq, Eq)]
pub struct AtomPath {
    home: bool,
    path: String,
}

/// [`AtomPath`] as the book stores it.
#[derive(Serialize, Deserialize)]
struct PathWire {
    home: bool,
    path: String,
}

impl AtomPath {
    /// Whether it is under the home folder (`~/` on the wire).
    #[must_use]
    pub const fn home_relative(&self) -> bool {
        self.home
    }

    /// The path: below the home folder where [`Self::home_relative`], else as given.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.path
    }

    fn checked(home: bool, path: String) -> Result<Self, WakeUsage> {
        if path.is_empty() {
            return Err(WakeUsage::Grammar);
        }
        if path.chars().any(char::is_control) {
            return Err(WakeUsage::Control);
        }
        if path.len() > PATH_MAX {
            return Err(WakeUsage::PathLong);
        }
        Ok(Self { home, path })
    }
}

impl fmt::Debug for AtomPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let base = if self.home { "~/" } else { "" };
        write!(f, "AtomPath({base}{} bytes)", self.path.len())
    }
}

impl Serialize for AtomPath {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        PathWire {
            home: self.home,
            path: self.path.clone(),
        }
        .serialize(s)
    }
}

impl<'de> Deserialize<'de> for AtomPath {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let wire = PathWire::deserialize(d)?;
        Self::checked(wire.home, wire.path).map_err(|why| serde::de::Error::custom(why.words()))
    }
}

/// A PATTERN (`file:PATH~/RE/`), percent-decoded.
#[derive(Clone, PartialEq, Eq)]
pub struct Pattern(String);

impl Pattern {
    fn checked(text: String) -> Result<Self, WakeUsage> {
        if text.is_empty() {
            return Err(WakeUsage::Grammar);
        }
        if text.chars().any(char::is_control) {
            return Err(WakeUsage::Control);
        }
        if text.chars().count() > PATTERN_MAX {
            return Err(WakeUsage::PatternLong);
        }
        aterm_observe::row_matcher(&text).map_err(|_| WakeUsage::Pattern)?;
        Ok(Self(text))
    }

    /// The pattern.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Pattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Pattern({} chars)", self.0.chars().count())
    }
}

impl Serialize for Pattern {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Pattern {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        checked_str(d, Self::checked)
    }
}

/// MACHINE TERMS OF THE ONE GRAMMAR (`await sys`'s), parsed, with their canonical spelling.
#[derive(Clone, PartialEq)]
pub struct SysWait {
    wait: AwaitSys,
    canonical: String,
}

impl SysWait {
    /// Parse `terms` into a machine wait as `await sys` reads `sys <terms…>`.
    /// # Errors
    /// [`WakeUsage`]: a volume term, a job's term, a level by name, or a form `await sys` refuses.
    pub fn parse(terms: &[&str]) -> Result<Self, WakeUsage> {
        let text = format!("sys {}", terms.join(" "));
        let wait = match AwaitSys::parse(&text) {
            Ok(Parsed::Machine(wait)) => wait,
            Ok(Parsed::Volume(_)) => return Err(WakeUsage::Volume),
            Err(reply) if reply == aterm_vitals::sys::predicate::MIXED => {
                return Err(WakeUsage::Volume);
            }
            Err(_) => return Err(WakeUsage::Grammar),
        };
        for term in &wait.terms {
            match &term.metric {
                Metric::Job(_) => return Err(WakeUsage::JobTerm),
                Metric::CpuLevel { index: None, .. } => return Err(WakeUsage::CpuName),
                _ => {}
            }
        }
        let canonical = wait.canonical();
        Ok(Self { wait, canonical })
    }

    /// Parse a stored `sys …` spelling ([`Self::canonical`]) under [`Self::parse`] rules.
    /// # Errors
    /// [`WakeUsage`]: a spelling that is not `sys` and terms [`Self::parse`] reads.
    pub fn of_spelling(text: &str) -> Result<Self, WakeUsage> {
        match text.split_whitespace().collect::<Vec<_>>().split_first() {
            Some((&"sys", terms)) => Self::parse(terms),
            _ => Err(WakeUsage::Grammar),
        }
    }

    /// The wait.
    #[must_use]
    pub const fn wait(&self) -> &AwaitSys {
        &self.wait
    }

    /// Its canonical spelling (`sys …`).
    #[must_use]
    pub fn canonical(&self) -> &str {
        &self.canonical
    }

    /// Whether it names build slots.
    #[must_use]
    pub fn names_slots(&self) -> bool {
        self.wait.terms.iter().any(|t| t.metric == Metric::Slots)
    }
}

impl fmt::Debug for SysWait {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SysWait({})", self.canonical)
    }
}

impl Serialize for SysWait {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.canonical)
    }
}

impl<'de> Deserialize<'de> for SysWait {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        checked_str(d, |text| Self::of_spelling(&text))
    }
}

fn checked_str<'de, D: serde::Deserializer<'de>, T>(
    d: D,
    check: impl FnOnce(String) -> Result<T, WakeUsage>,
) -> Result<T, D::Error> {
    check(String::deserialize(d)?).map_err(|why| serde::de::Error::custom(why.words()))
}

/// Source atom: a condition of the agent's files or processes, judged by level and latched by edge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceAtom {
    /// `file:PATH` (the file is there) or `file:PATH~/RE/` (a line of it matches).
    File {
        /// The path.
        path: AtomPath,
        /// The pattern.
        pattern: Option<Pattern>,
    },
    /// `gone:PATH`: nothing is there.
    Gone {
        /// The path.
        path: AtomPath,
    },
    /// `pid:N exited`: a process of the session's tree is gone (a lasting level).
    Pid {
        /// The pid.
        pid: u32,
    },
}

impl SourceAtom {
    /// What it reads.
    #[must_use]
    pub const fn kind(&self) -> AtomKind {
        match self {
            SourceAtom::File { pattern: None, .. } => AtomKind::File,
            SourceAtom::File {
                pattern: Some(_), ..
            } => AtomKind::Match,
            SourceAtom::Gone { .. } => AtomKind::Gone,
            SourceAtom::Pid { .. } => AtomKind::Pid,
        }
    }
}

/// A TIME ATOM, as declared; the book resolves it to an instant at the declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TimeAtom {
    /// `at:HH:MM`: the next such local time.
    At {
        /// The hour, 0 to 23.
        hh: u8,
        /// The minute, 0 to 59.
        mm: u8,
    },
    /// `in:DUR`: so long after the declaration.
    In {
        /// The span, ms.
        ms: i64,
    },
}

/// ONE ATOM OF A WAKE'S CONDITION.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Atom {
    /// A source atom.
    Source(SourceAtom),
    /// A time atom (a wake's own `when=` alone).
    Time(TimeAtom),
    /// The `when=`'s machine terms, together, as one atom.
    Sys(SysWait),
}

impl Atom {
    /// What it reads.
    #[must_use]
    pub const fn kind(&self) -> AtomKind {
        match self {
            Atom::Source(source) => source.kind(),
            Atom::Time(TimeAtom::At { .. }) => AtomKind::At,
            Atom::Time(TimeAtom::In { .. }) => AtomKind::In,
            Atom::Sys(_) => AtomKind::Sys,
        }
    }
}

/// A DECLARATION, read ([`Self::parse`]).
#[derive(Debug, Clone, PartialEq)]
pub struct WakeReq {
    /// Its label.
    pub label: Token,
    /// The condition's atoms, all of which must hold, in their order.
    pub when: Vec<Atom>,
    /// What must hold when it is told.
    pub need: Option<SysWait>,
    /// Atoms any one of which, holding, ends it untold.
    pub unless: Vec<SourceAtom>,
    /// Its priority.
    pub prio: Prio,
    /// It is due within this of its declaration, ms.
    pub by_ms: i64,
}

/// Which predicate a term joins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    When,
    Need,
}

enum Read {
    Source(SourceAtom),
    Time(TimeAtom),
    Sys(String),
}

/// A predicate being read: its atoms, its machine terms and their words.
#[derive(Default)]
struct Pred {
    atoms: Vec<Atom>,
    sys: Vec<String>,
    sys_at: Option<usize>,
    words: Vec<String>,
}

impl Pred {
    fn term(&mut self, read: Read, place: Place) -> Result<(), WakeUsage> {
        match (read, place) {
            (Read::Sys(term), Place::When | Place::Need) => {
                self.sys_at.get_or_insert(self.atoms.len());
                self.sys.push(term);
            }
            (Read::Source(source), Place::When) => self.atoms.push(Atom::Source(source)),
            (Read::Time(time), Place::When) => self.atoms.push(Atom::Time(time)),
            (Read::Source(_) | Read::Time(_), Place::Need) => return Err(WakeUsage::NeedSys),
        }
        if self.atoms.len() + usize::from(self.sys_at.is_some()) > WHEN_MAX {
            return Err(WakeUsage::TooMany);
        }
        Ok(())
    }

    fn machine(&self) -> Result<Option<SysWait>, WakeUsage> {
        if self.sys.is_empty() {
            return if self.words.is_empty() {
                Ok(None)
            } else {
                Err(WakeUsage::ForWithoutSys)
            };
        }
        let mut terms: Vec<&str> = Vec::new();
        for (at, term) in self.sys.iter().enumerate() {
            if at > 0 {
                terms.push("and");
            }
            terms.push(term);
        }
        terms.extend(self.words.iter().map(String::as_str));
        SysWait::parse(&terms).map(Some)
    }
}

impl WakeReq {
    /// THE DECLARATION `rest` SPELLS (the words after `wake`).
    /// # Errors
    /// [`WakeUsage`]: the first reason the words are not a declaration.
    pub fn parse(rest: &str) -> Result<Self, WakeUsage> {
        let mut toks = rest.split_whitespace();
        let mut label: Option<Token> = None;
        let mut when: Option<Pred> = None;
        let mut need: Option<Pred> = None;
        let mut unless: Vec<SourceAtom> = Vec::new();
        let mut prio: Option<Prio> = None;
        let mut by_ms: Option<i64> = None;
        let mut open: Option<Place> = None;
        while let Some(tok) = toks.next() {
            if tok.chars().any(char::is_control) {
                return Err(WakeUsage::Control);
            }
            if tok == "and" {
                let next = toks.next().ok_or(WakeUsage::Grammar)?;
                let (pred, place) = match open {
                    Some(Place::When) => (when.as_mut(), Place::When),
                    Some(Place::Need) => (need.as_mut(), Place::Need),
                    None => (None, Place::When),
                };
                let pred = pred.ok_or(WakeUsage::Grammar)?;
                pred.term(read(next, &mut toks)?, place)?;
                continue;
            }
            if tok == "irreversible" {
                return Err(WakeUsage::Slice4);
            }
            let (key, value) = tok.split_once('=').ok_or(WakeUsage::Grammar)?;
            match key {
                "label" if label.is_none() => {
                    label = Some(Token::parse(value).ok_or(WakeUsage::Label)?);
                    open = None;
                }
                "when" if when.is_none() => {
                    let mut pred = Pred::default();
                    pred.term(read(value, &mut toks)?, Place::When)?;
                    when = Some(pred);
                    open = Some(Place::When);
                }
                "need" if need.is_none() => {
                    let mut pred = Pred::default();
                    pred.term(read(value, &mut toks)?, Place::Need)?;
                    need = Some(pred);
                    open = Some(Place::Need);
                }
                "unless" => {
                    match read(value, &mut toks)? {
                        Read::Source(source) => unless.push(source),
                        Read::Time(_) | Read::Sys(_) => return Err(WakeUsage::UnlessSource),
                    }
                    if unless.len() > UNLESS_MAX {
                        return Err(WakeUsage::TooMany);
                    }
                    open = None;
                }
                "prio" if prio.is_none() => {
                    prio = Some(Prio::of_word(value).ok_or(WakeUsage::Grammar)?);
                    open = None;
                }
                "by" if by_ms.is_none() => {
                    let span = value.strip_prefix('+').ok_or(WakeUsage::By)?;
                    by_ms = Some(span_ms(span).ok_or(WakeUsage::By)?);
                    open = None;
                }
                "for" | "scope" => {
                    let pred = match open {
                        Some(Place::When) => when.as_mut(),
                        Some(Place::Need) => need.as_mut(),
                        None => None,
                    }
                    .ok_or(WakeUsage::ForWithoutSys)?;
                    if pred.sys.is_empty() {
                        return Err(WakeUsage::ForWithoutSys);
                    }
                    pred.words.push(tok.to_string());
                }
                "timeout" => return Err(WakeUsage::Timeout),
                "vol" => return Err(WakeUsage::Volume),
                _ => return Err(WakeUsage::Grammar),
            }
        }
        let label = label.ok_or(WakeUsage::Label)?;
        let when = when.ok_or(WakeUsage::NoWhen)?;
        let mut atoms = when.atoms.clone();
        if let Some(sys) = when.machine()? {
            atoms.insert(when.sys_at.unwrap_or(atoms.len()), Atom::Sys(sys));
        }
        let need = match need {
            Some(pred) => pred.machine()?,
            None => None,
        };
        Ok(Self {
            label,
            when: atoms,
            need,
            unless,
            prio: prio.unwrap_or(Prio::Normal),
            by_ms: by_ms.unwrap_or(BY_DEFAULT_MS),
        })
    }

    /// Whether its condition is time alone.
    #[must_use]
    pub fn await_only(&self) -> bool {
        self.when.iter().all(|a| matches!(a, Atom::Time(_)))
    }
}

fn read<'a>(value: &str, toks: &mut impl Iterator<Item = &'a str>) -> Result<Read, WakeUsage> {
    if value.is_empty() {
        return Err(WakeUsage::Grammar);
    }
    if let Some(raw) = value.strip_prefix("file:") {
        let (path, pattern) = path_and_pattern(raw, true)?;
        return Ok(Read::Source(SourceAtom::File { path, pattern }));
    }
    if let Some(raw) = value.strip_prefix("gone:") {
        let (path, _) = path_and_pattern(raw, false)?;
        return Ok(Read::Source(SourceAtom::Gone { path }));
    }
    if let Some(digits) = value.strip_prefix("pid:") {
        let canonical = (1..=10).contains(&digits.len())
            && digits.bytes().all(|b| b.is_ascii_digit())
            && !digits.starts_with('0');
        let pid = digits
            .parse::<u32>()
            .ok()
            .filter(|_| canonical)
            .ok_or(WakeUsage::Grammar)?;
        if toks.next() != Some("exited") {
            return Err(WakeUsage::Grammar);
        }
        return Ok(Read::Source(SourceAtom::Pid { pid }));
    }
    if let Some(clock) = value.strip_prefix("at:") {
        let (hh, mm) = clock.split_once(':').ok_or(WakeUsage::Grammar)?;
        let two = |t: &str, below: u8| {
            (t.len() == 2 && t.bytes().all(|b| b.is_ascii_digit()))
                .then(|| t.parse::<u8>().ok())
                .flatten()
                .filter(|n| *n < below)
        };
        return match (two(hh, 24), two(mm, 60)) {
            (Some(hh), Some(mm)) => Ok(Read::Time(TimeAtom::At { hh, mm })),
            _ => Err(WakeUsage::Grammar),
        };
    }
    if let Some(span) = value.strip_prefix("in:") {
        return Ok(Read::Time(TimeAtom::In {
            ms: span_ms(span).ok_or(WakeUsage::By)?,
        }));
    }
    if value.starts_with("task:") {
        return Err(WakeUsage::AwaitTask);
    }
    if value.starts_with("git(") || value.starts_with("atpkg(") || value == "workflow.returned" {
        return Err(WakeUsage::Slice3);
    }
    if value.starts_with("claim=") || value.starts_with("claim.") {
        return Err(WakeUsage::Slice4);
    }
    if value.starts_with("free") || value.starts_with("disk=") || value.starts_with("vol=") {
        return Err(WakeUsage::Volume);
    }
    if value.starts_with("job=") || value.starts_with("task=") {
        return Err(WakeUsage::JobTerm);
    }
    Ok(Read::Sys(value.to_string()))
}

fn span_ms(text: &str) -> Option<i64> {
    let digits = text.bytes().take_while(u8::is_ascii_digit).count();
    let (n, unit) = text.split_at(digits);
    if !(1..=6).contains(&n.len()) {
        return None;
    }
    let scale = match unit {
        "s" => 1_000,
        "m" => MINUTE_MS,
        "h" => HOUR_MS,
        _ => return None,
    };
    let ms = n.parse::<i64>().ok()?.checked_mul(scale)?;
    (1..=BY_MAX_MS).contains(&ms).then_some(ms)
}

fn path_and_pattern(raw: &str, pattern_ok: bool) -> Result<(AtomPath, Option<Pattern>), WakeUsage> {
    let (home, body) = match raw.strip_prefix("~/") {
        Some(rest) => (true, rest),
        None => (false, raw),
    };
    let (path_raw, pattern_raw) = match body.find("~/") {
        Some(at) => (&body[..at], Some(&body[at + 2..])),
        None => (body, None),
    };
    let path = AtomPath::checked(home, decode(path_raw)?)?;
    let pattern = match pattern_raw {
        None => None,
        Some(_) if !pattern_ok => return Err(WakeUsage::Grammar),
        Some(raw) => {
            let inner = raw.strip_suffix('/').ok_or(WakeUsage::Grammar)?;
            if inner.contains('/') {
                return Err(WakeUsage::Grammar);
            }
            Some(Pattern::checked(decode(inner)?)?)
        }
    };
    Ok((path, pattern))
}

fn decode(raw: &str) -> Result<String, WakeUsage> {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let hex = |b: Option<&u8>| b.and_then(|b| char::from(*b).to_digit(16));
            match (hex(bytes.get(at + 1)), hex(bytes.get(at + 2))) {
                (Some(hi), Some(lo)) => {
                    out.push(u8::try_from(hi * 16 + lo).map_err(|_| WakeUsage::Escape)?);
                    at += 3;
                    continue;
                }
                _ => return Err(WakeUsage::Escape),
            }
        }
        out.push(bytes[at]);
        at += 1;
    }
    String::from_utf8(out).map_err(|_| WakeUsage::Escape)
}

/// ONE ATOM OF A DECLARED WAKE'S CONDITION, with its instant (a time atom's) and whether it holds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WhenAtom {
    /// The atom.
    pub atom: Atom,
    /// A time atom's instant, unix ms.
    pub at_ms: Option<i64>,
    /// It holds.
    pub held: bool,
}

/// ONE DECLARED WAKE.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeclaredWake {
    /// Its ordinal in the session's book, from 1.
    pub ord: u16,
    /// Its label.
    pub label: Token,
    /// Who declared it.
    pub by: Declarer,
    /// The agent process it is declared for: gone, it expires.
    pub agent: AgentId,
    /// Its condition.
    pub when: Vec<WhenAtom>,
    /// What must hold when it is told.
    pub need: Option<SysWait>,
    /// Atoms any one of which, holding, ends it untold.
    pub unless: Vec<SourceAtom>,
    /// Its priority.
    pub prio: Prio,
    /// When it was declared, unix ms.
    pub declared_ms: i64,
    /// When it expires untold, unix ms.
    pub due_by_ms: i64,
    /// Its state.
    pub state: WakeState,
    /// When its condition first held, unix ms.
    pub ready_since_ms: Option<i64>,
    /// When an `await wake` answer for it was written, unix ms.
    pub answered_ms: Option<i64>,
    /// Its condition is time alone.
    pub await_only: bool,
}

impl DeclaredWake {
    /// Its episode ([`wake_episode`]).
    #[must_use]
    pub fn episode(&self) -> u64 {
        wake_episode(self.declared_ms, self.ord)
    }

    const fn open(&self) -> bool {
        matches!(self.state, WakeState::Pending | WakeState::Ready)
    }

    fn ready_at(&mut self, now_ms: i64) -> bool {
        if self.state == WakeState::Pending && self.when.iter().all(|a| a.held) {
            self.state = WakeState::Ready;
            self.ready_since_ms = Some(now_ms);
            return true;
        }
        false
    }

    fn seen(&self) -> WakeSeen {
        WakeSeen {
            ord: self.ord,
            class: self.by,
            prio: self.prio,
            state: self.state,
            declared_ms: self.declared_ms,
            ready_since_ms: self.ready_since_ms,
            kinds: self.when.iter().map(|a| a.atom.kind()).collect(),
            need: self.need.as_ref().map(|n| n.canonical().to_string()),
            need_slots: self.need.as_ref().is_some_and(SysWait::names_slots),
            await_only: self.await_only,
        }
    }
}

/// Why a declaration was not booked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclareRefusal {
    /// The book holds [`MAX_WAKES`] wakes still to be told.
    Full,
    /// A time atom falls after its `by=`.
    PastBy,
    /// The session's ordinals are spent.
    Spent,
}

impl DeclareRefusal {
    /// Its words (`ERR denied wake: <words>`).
    #[must_use]
    pub const fn words(self) -> &'static str {
        match self {
            DeclareRefusal::Full => "16 wakes are pending",
            DeclareRefusal::PastBy => "a time falls after by=",
            DeclareRefusal::Spent => "the session's ordinals are spent",
        }
    }
}

/// What a look or an edge changed of one wake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// Its condition holds: due to be told.
    Ready,
    /// It ended untold.
    Expired(ExpireWhy),
    /// An answer to it ends told (its `by=` passed, or its agent went, after the answer).
    Told(WakeDoor),
}

/// What a level read says, by the caller ([`WakeBook::on_look`]).
pub trait Levels {
    /// Whether `atom` holds.
    fn source(&self, atom: &SourceAtom) -> Option<bool>;
    /// Whether `wait` holds (its `for=` judged by the caller).
    fn sys(&self, wait: &SysWait) -> Option<bool>;
    /// Whether `agent` is still the process it was (its pid alive, born when it was).
    fn agent_alive(&self, agent: AgentId) -> bool;
}

/// What a successor's rows make of a claim its predecessor held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reconciled {
    /// The last row says it was typed: told.
    Told,
    /// An intent with no outcome, a skip, or no row: released, so it may be told.
    Released,
}

/// THE RECONCILE OF A CLAIM ON `episode` from the ledger's fact rows (oldest first).
#[must_use]
pub fn reconcile_claim(episode: u64, rows: &[(u64, SeedOutcome)]) -> Reconciled {
    use super::fault::{Fault, armed};
    let last = rows
        .iter()
        .rev()
        .find(|(of, _)| *of == episode)
        .map(|(_, outcome)| *outcome);
    match last {
        Some(SeedOutcome::Typed) if !armed(Fault::ReleaseAfterTyped) => Reconciled::Told,
        _ => Reconciled::Released,
    }
}

/// ONE SESSION'S BOOK ([module docs](self)).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WakeBook {
    /// Its wakes, lowest ordinal first.
    pub wakes: Vec<DeclaredWake>,
    /// The ordinal the next declaration takes.
    pub next_ord: u16,
}

impl Default for WakeBook {
    fn default() -> Self {
        Self {
            wakes: Vec::new(),
            next_ord: 1,
        }
    }
}

impl WakeBook {
    /// `ord`'s wake.
    #[must_use]
    pub fn get(&self, ord: u16) -> Option<&DeclaredWake> {
        self.wakes.iter().find(|w| w.ord == ord)
    }

    fn get_mut(&mut self, ord: u16) -> Option<&mut DeclaredWake> {
        self.wakes.iter_mut().find(|w| w.ord == ord)
    }

    /// THE DECLARATION `req`, by `by` for `agent`, at `now_ms` in a zone `local_offset_s` from UTC.
    /// # Errors
    /// [`DeclareRefusal`].
    pub fn declare(
        &mut self,
        req: WakeReq,
        by: Declarer,
        agent: AgentId,
        now_ms: i64,
        local_offset_s: i64,
    ) -> Result<u16, DeclareRefusal> {
        let due_by_ms = now_ms.saturating_add(req.by_ms);
        let await_only = req.await_only();
        let mut when = Vec::with_capacity(req.when.len());
        for atom in req.when {
            let at_ms = match atom {
                Atom::Time(TimeAtom::In { ms }) => Some(now_ms.saturating_add(ms)),
                Atom::Time(TimeAtom::At { hh, mm }) => {
                    let local = now_ms
                        .saturating_add(local_offset_s.saturating_mul(1_000))
                        .rem_euclid(DAY_MS);
                    let target = i64::from(hh) * HOUR_MS + i64::from(mm) * MINUTE_MS;
                    Some(now_ms.saturating_add((target - local).rem_euclid(DAY_MS)))
                }
                Atom::Source(_) | Atom::Sys(_) => None,
            };
            if at_ms.is_some_and(|at| at > due_by_ms) {
                return Err(DeclareRefusal::PastBy);
            }
            when.push(WhenAtom {
                atom,
                at_ms,
                held: false,
            });
        }
        if self.wakes.iter().filter(|w| w.state.live()).count() >= MAX_WAKES {
            return Err(DeclareRefusal::Full);
        }
        let ord = self.next_ord.max(1);
        let next = ord.checked_add(1).ok_or(DeclareRefusal::Spent)?;
        if self.wakes.len() >= MAX_WAKES
            && let Some(at) = self.wakes.iter().position(|w| !w.state.live())
        {
            self.wakes.remove(at);
        }
        self.next_ord = next;
        self.wakes.push(DeclaredWake {
            ord,
            label: req.label,
            by,
            agent,
            when,
            need: req.need,
            unless: req.unless,
            prio: req.prio,
            declared_ms: now_ms,
            due_by_ms,
            state: WakeState::Pending,
            ready_since_ms: None,
            answered_ms: None,
            await_only,
        });
        Ok(ord)
    }

    /// AN EDGE ON SOURCE ATOM `atom` (its index in the condition) of wake `ord` at `now_ms`.
    pub fn on_source(&mut self, ord: u16, atom: usize, now_ms: i64) -> Option<Change> {
        use super::fault::{Fault, armed};
        if armed(Fault::LevelOnly) {
            return None;
        }
        let wake = self.get_mut(ord)?;
        if wake.state != WakeState::Pending {
            return None;
        }
        let when = wake.when.get_mut(atom)?;
        if !matches!(when.atom, Atom::Source(_)) {
            return None;
        }
        when.held = true;
        wake.ready_at(now_ms).then_some(Change::Ready)
    }

    /// THE LOOK AT `now_ms`, every level read again by `levels`.
    pub fn on_look(&mut self, levels: &dyn Levels, now_ms: i64) -> Vec<(u16, Change)> {
        let mut changed = Vec::new();
        for wake in &mut self.wakes {
            match wake.state {
                WakeState::Answering { .. } | WakeState::Claimed { .. } => {
                    let why = if now_ms >= wake.due_by_ms {
                        Some(ExpireWhy::Due)
                    } else if !levels.agent_alive(wake.agent) {
                        Some(ExpireWhy::Agent)
                    } else {
                        None
                    };
                    if let Some(why) = why {
                        wake.state = WakeState::Expired { why };
                        changed.push((wake.ord, Change::Expired(why)));
                    }
                }
                WakeState::Answered { .. } => {
                    if now_ms >= wake.due_by_ms || !levels.agent_alive(wake.agent) {
                        wake.state = WakeState::Told {
                            door: WakeDoor::Await,
                            at_ms: now_ms,
                        };
                        changed.push((wake.ord, Change::Told(WakeDoor::Await)));
                    }
                }
                WakeState::Pending | WakeState::Ready => {
                    let why = if now_ms >= wake.due_by_ms {
                        Some(ExpireWhy::Due)
                    } else if !levels.agent_alive(wake.agent) {
                        Some(ExpireWhy::Agent)
                    } else if wake.unless.iter().any(|a| levels.source(a) == Some(true)) {
                        Some(ExpireWhy::Unless)
                    } else {
                        None
                    };
                    if let Some(why) = why {
                        wake.state = WakeState::Expired { why };
                        changed.push((wake.ord, Change::Expired(why)));
                        continue;
                    }
                    if wake.state == WakeState::Ready {
                        continue;
                    }
                    for when in &mut wake.when {
                        when.held = match &when.atom {
                            Atom::Source(source) => {
                                when.held || levels.source(source) == Some(true)
                            }
                            Atom::Time(_) => when.at_ms.is_some_and(|at| now_ms >= at),
                            Atom::Sys(wait) => levels.sys(wait) == Some(true),
                        };
                    }
                    if wake.ready_at(now_ms) {
                        changed.push((wake.ord, Change::Ready));
                    }
                }
                WakeState::Told { .. } | WakeState::Taken | WakeState::Expired { .. } => {}
            }
        }
        changed
    }

    /// THE `await wake` ANSWER BEGINS for ready wake `ord`: `Answering` while the reply is written.
    pub fn answer_begin(&mut self, ord: u16, now_ms: i64) -> bool {
        match self.get_mut(ord) {
            Some(wake) if wake.state == WakeState::Ready => {
                wake.state = WakeState::Answering { at_ms: now_ms };
                true
            }
            _ => false,
        }
    }

    /// THE ANSWER'S WRITE ENDED, `ok` or not.
    pub fn answer_done(&mut self, ord: u16, ok: bool, hosted: bool, now_ms: i64) {
        use super::fault::{Fault, armed};
        let Some(wake) = self.get_mut(ord) else {
            return;
        };
        if !matches!(wake.state, WakeState::Answering { .. }) {
            return;
        }
        wake.state = if ok || armed(Fault::ToldBeforeWrite) {
            wake.answered_ms = Some(now_ms);
            if hosted {
                WakeState::Answered { at_ms: now_ms }
            } else {
                WakeState::Told {
                    door: WakeDoor::Await,
                    at_ms: now_ms,
                }
            }
        } else {
            WakeState::Ready
        };
    }

    /// THE ANSWER TO `ord` WAS HEARD (the loop saw a turn run after it): told by the answer.
    pub fn heard(&mut self, ord: u16, now_ms: i64) -> bool {
        match self.get_mut(ord) {
            Some(wake) if matches!(wake.state, WakeState::Answered { .. }) => {
                wake.state = WakeState::Told {
                    door: WakeDoor::Await,
                    at_ms: now_ms,
                };
                true
            }
            _ => false,
        }
    }

    /// A LOOP'S DOOR CLAIMS wake `ord` of `episode` for its one write at `now_ms`.
    /// A time-only wake is `Gone` to a loop's door: only its await may answer it.
    pub fn claim(&mut self, ord: u16, episode: u64, door: WakeDoor, now_ms: i64) -> WakeClaim {
        use super::fault::{Fault, armed};
        let Some(wake) = self.get_mut(ord).filter(|w| w.episode() == episode) else {
            return WakeClaim::Gone;
        };
        if door == WakeDoor::Await {
            return WakeClaim::Busy;
        }
        if wake.await_only {
            return WakeClaim::Gone;
        }
        let claimed = WakeState::Claimed {
            door,
            at_ms: now_ms,
        };
        match wake.state {
            WakeState::Ready => {
                wake.state = claimed;
                WakeClaim::Granted
            }
            WakeState::Answered { at_ms } if now_ms.saturating_sub(at_ms) >= UNHEARD_MS => {
                wake.state = claimed;
                WakeClaim::Granted
            }
            WakeState::Answered { .. } => WakeClaim::Answered,
            WakeState::Answering { .. } | WakeState::Claimed { .. } => WakeClaim::Busy,
            WakeState::Pending => WakeClaim::Unmet,
            WakeState::Told { .. } if armed(Fault::ClaimTwice) => {
                wake.state = claimed;
                WakeClaim::Granted
            }
            WakeState::Told { .. } | WakeState::Taken | WakeState::Expired { .. } => {
                WakeClaim::Gone
            }
        }
    }

    /// THE LOOP RELEASES its claim on `ord` of `episode`, its line not typed: ready again.
    pub fn release(&mut self, ord: u16, episode: u64) -> bool {
        match self.get_mut(ord).filter(|w| w.episode() == episode) {
            Some(wake) if matches!(wake.state, WakeState::Claimed { .. }) => {
                wake.state = match wake.answered_ms {
                    Some(at_ms) => WakeState::Answered { at_ms },
                    None => WakeState::Ready,
                };
                true
            }
            _ => false,
        }
    }

    /// `ord` of `episode` WAS TOLD through `door` at `now_ms`.
    pub fn told(&mut self, ord: u16, episode: u64, door: WakeDoor, now_ms: i64) -> bool {
        if self.get(ord).is_none_or(|w| w.episode() != episode) {
            return false;
        }
        if door == WakeDoor::Await {
            return self.heard(ord, now_ms);
        }
        match self.get_mut(ord) {
            Some(wake) if matches!(wake.state, WakeState::Claimed { door: d, .. } if d == door) => {
                wake.state = WakeState::Told {
                    door,
                    at_ms: now_ms,
                };
                true
            }
            _ => false,
        }
    }

    /// THE AGENT REMOVES `ord` (`wake rm #<n>`): gone from the book.
    pub fn rm(&mut self, ord: u16) -> bool {
        let before = self.wakes.len();
        self.wakes.retain(|w| w.ord != ord);
        self.wakes.len() != before
    }

    /// THE AGENT TOOK `ord` (`wake taken #<n>`): a wake still to be told, or told, is `Taken`.
    pub fn taken(&mut self, ord: u16) -> bool {
        match self.get_mut(ord) {
            Some(wake) if wake.state.live() || matches!(wake.state, WakeState::Told { .. }) => {
                wake.state = WakeState::Taken;
                true
            }
            _ => false,
        }
    }

    /// EVERY WAKE STILL TO BE TOLD EXPIRES for `why` (the owner switched occupancy off).
    pub fn disarm(&mut self, why: ExpireWhy) -> Vec<u16> {
        let mut ended = Vec::new();
        for wake in &mut self.wakes {
            if wake.state.live() {
                wake.state = WakeState::Expired { why };
                ended.push(wake.ord);
            }
        }
        ended
    }

    /// A SUCCESSOR'S RECONCILE of the claims a loop's door held, from the ledger's fact rows.
    pub fn reconcile(
        &mut self,
        rows: &[(u64, SeedOutcome)],
        now_ms: i64,
    ) -> Vec<(u16, Reconciled)> {
        let claims: Vec<(u16, u64, WakeDoor)> = self
            .wakes
            .iter()
            .filter_map(|w| match w.state {
                WakeState::Claimed { door, .. } => Some((w.ord, w.episode(), door)),
                _ => None,
            })
            .collect();
        claims
            .into_iter()
            .map(|(ord, episode, door)| {
                let outcome = reconcile_claim(episode, rows);
                match outcome {
                    Reconciled::Told => {
                        self.told(ord, episode, door, now_ms);
                    }
                    Reconciled::Released => {
                        self.release(ord, episode);
                    }
                }
                (ord, outcome)
            })
            .collect()
    }

    /// THE BOOK AS A SUCCESSOR WINDOW READS IT BACK from a session record.
    #[must_use]
    pub fn restored(mut self) -> Self {
        for wake in &mut self.wakes {
            if matches!(wake.state, WakeState::Answering { .. }) {
                wake.state = WakeState::Ready;
            }
        }
        self
    }

    /// THE WAKE A LOOP'S DOOR WOULD TELL FIRST ([`WakesSeen::head_due`]).
    #[must_use]
    pub fn head_due(&self) -> Option<u16> {
        self.seen().head_due().map(|w| w.ord)
    }

    /// THE READY WAKES A PARKED `await wake` MAY BE ANSWERED BY, in the order it takes them.
    #[must_use]
    pub fn for_await(&self) -> Vec<u16> {
        let mut ready: Vec<&DeclaredWake> = self
            .wakes
            .iter()
            .filter(|w| w.state == WakeState::Ready)
            .collect();
        ready.sort_by_key(|w| head_key(w.prio, w.ord));
        ready.into_iter().map(|w| w.ord).collect()
    }

    /// Whether a wake is open to an answer (pending or ready).
    #[must_use]
    pub fn any_open(&self) -> bool {
        self.wakes.iter().any(DeclaredWake::open)
    }

    /// THE BOOK AS THE LOOP SEES IT: each wake still to be told, closed words and numbers.
    #[must_use]
    pub fn seen(&self) -> WakesSeen {
        WakesSeen {
            wakes: self
                .wakes
                .iter()
                .filter(|w| w.state.live())
                .map(DeclaredWake::seen)
                .collect(),
        }
    }
}

/// How a parked wait's last open wake ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// It expired.
    Expired(ExpireWhy),
    /// The agent removed it.
    Removed,
    /// The agent took it.
    Taken,
    /// A loop's door told it.
    Told,
}

impl Ended {
    /// Its word (`why=<word>`).
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Ended::Expired(why) => why.word(),
            Ended::Removed => "rm",
            Ended::Taken => "taken",
            Ended::Told => "told",
        }
    }
}

/// ONE PARKED `await wake`: the connection it waits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Park {
    /// The connection.
    pub conn: u64,
    /// When it parked, unix ms.
    pub since_ms: i64,
}

/// Why an `await wake` does not park.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParkRefusal {
    /// The session's one park stands.
    Busy,
    /// No wake is open to answer it.
    NonePending,
}

impl ParkRefusal {
    /// Its reply.
    #[must_use]
    pub const fn reply(self) -> &'static str {
        match self {
            ParkRefusal::Busy => "ERR busy await wake parked\n",
            ParkRefusal::NonePending => "ERR usage await wake: no wake pending\n",
        }
    }
}

/// THE ANSWER A PARKED WAIT GETS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParkAnswer {
    /// Wake `ord` is ready, since `since_ms`.
    Ready {
        /// The wake.
        ord: u16,
        /// When its condition first held, unix ms.
        since_ms: i64,
        /// The build slots usable of those present, where its need names them.
        slots: Option<(u16, u16)>,
    },
    /// Its last open wake ended untold.
    Ended {
        /// The wake.
        ord: u16,
        /// How.
        why: Ended,
    },
}

impl ParkAnswer {
    /// Format the reply (`OK wake #<n> ready since=<ms>` or `EXPIRED wake #<n> why=<word>`).
    #[must_use]
    pub fn reply(self) -> String {
        match self {
            ParkAnswer::Ready {
                ord,
                since_ms,
                slots,
            } => {
                let slots = slots.map_or(String::new(), |(u, n)| format!(" slots={u}/{n}"));
                format!("OK wake #{ord} ready since={since_ms}{slots}\n")
            }
            ParkAnswer::Ended { ord, why } => format!("EXPIRED wake #{ord} why={}\n", why.word()),
        }
    }
}

/// THE SESSION'S ONE PARKED `await wake` (one of the 64 shared wait slots at most).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParkBook {
    parks: Vec<Park>,
}

impl ParkBook {
    /// `conn` PARKS on `book` at `now_ms`.
    /// # Errors
    /// [`ParkRefusal::Busy`] while a park stands; [`ParkRefusal::NonePending`] for a book with no
    /// wake open.
    pub fn park(&mut self, conn: u64, book: &WakeBook, now_ms: i64) -> Result<(), ParkRefusal> {
        use super::fault::{Fault, armed};
        if !self.parks.is_empty() && !armed(Fault::ParkSecond) {
            return Err(ParkRefusal::Busy);
        }
        if !book.any_open() {
            return Err(ParkRefusal::NonePending);
        }
        self.parks.push(Park {
            conn,
            since_ms: now_ms,
        });
        Ok(())
    }

    /// The parks standing.
    #[must_use]
    pub fn parked(&self) -> &[Park] {
        &self.parks
    }

    /// `conn`'s park ends (answered, or its connection closed).
    pub fn unpark(&mut self, conn: u64) -> bool {
        let before = self.parks.len();
        self.parks.retain(|p| p.conn != conn);
        self.parks.len() != before
    }

    /// THE PARK'S END, once `book` holds no wake open to answer it.
    pub fn ended(&mut self, book: &WakeBook, ord: u16, why: Ended) -> Vec<(Park, ParkAnswer)> {
        if book.any_open() {
            return Vec::new();
        }
        std::mem::take(&mut self.parks)
            .into_iter()
            .map(|park| (park, ParkAnswer::Ended { ord, why }))
            .collect()
    }
}

#[cfg(test)]
#[path = "wakes_tests.rs"]
mod tests;
