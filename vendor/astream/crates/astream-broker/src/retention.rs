// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! RETENTION CLASSES: the rules a broker retains its log by, and the pure planning they
//! take (`REQUIREMENTS-fabric-operations` R4, "Design (2026-10-19)").
//!
//! # The rules
//!
//! A broker's retention is a list of [`Rule`]s, each `(filter, class, max_age, max_bytes)`,
//! plus a DEFAULT rule for every subject no rule names ([`Retention`]). A subject belongs to
//! the first rule whose filter matches it, and a broker-internal (hidden) subject to the
//! default rule, which counts its bytes and never holds it (its bindings, wills and group
//! commits are carried in `<log>.state`). A broker with no rules never compacts by itself.
//!
//! There are two classes:
//!
//! - **prefix**: exactly one rule, the bulk stream (`/f/<F>/pub/*/*/ev` in a fabric). It
//!   alone holds the CUT, the log's new base, down to the oldest record it still wants: the
//!   newest records of its class within its `max_bytes`, none older than its `max_age`.
//!   Below the cut its records are gone, so its floor is the base.
//! - **hold**: every other rule, the default one included. A hold rule never holds the cut
//!   back. Below the cut, each record a hold rule still wants is carried, by its absolute
//!   offset, in the held store (`held.rs`): one that keeps ALL ([`Keep::All`]) wants every
//!   record within its `max_age` that its readers have not read ([`Rule::until_read`]); one
//!   that keeps the LAST value only ([`Keep::Last`]) wants nothing beyond each subject's
//!   `Last` row, which every compaction carries in `<log>.state` whatever the rules.
//!
//! # Every class bounded
//!
//! A class's bytes are its records' in the retained suffix plus its held ones. A class past
//! its own `max_bytes` gives up its oldest: a hold class drops its oldest held records, and
//! when its records above the cut alone are past it, the cut rises to put the oldest of them
//! below, where they are dropped instead of carried. So a hold rule never holds the cut back
//! (one held mail cannot pin every subject's records, which a cut that is the minimum over
//! every rule's floor would), and the retained and held record bytes together stay within
//! the sum of the rules' `max_bytes`, plus what the log took since the planning: broker RAM
//! follows the rules, beside the `Last` rows the subject caps bound
//! ([`MAX_SUBJECTS_PER_PRODUCER`](crate::store::MAX_SUBJECTS_PER_PRODUCER)).
//!
//! A class past its bytes gives up AT LEAST A QUARTER of them ([`SLACK_DIV`]), its oldest,
//! or down to its bytes when it is further past them than that: so a class that has just
//! given records up has a quarter of its bytes of room, less what it was past them by,
//! before it is past them again. A cut
//! copies the log it keeps, so without that room a log at its bounds would be copied whole
//! for every batch it took; with it, a class's records ask for a cut at most once a quarter
//! of its bytes. A log far past its bounds keeps exactly its bytes (R4's acceptance: 100 of
//! 250 records, the oldest 151).
//!
//! The broker's writer asks for a tick when a batch takes SOME CLASS past the room the last
//! planning left it (`Plan::room`), counted by the class of each record it takes, never by
//! the log's length: a class at its bytes while another class's records arrive asks for
//! nothing until a record of its own does.
//!
//! # Told, never silent
//!
//! A record a [`Keep::All`] hold rule gave up (past its bytes or its age, or read by every
//! reader it names) raises its SUBJECT's floor past it; a `Fetch` below the floor of what
//! its filter can match answers `ERR retained-from=<floor>` (`proto::RETAINED_FROM_PREFIX`)
//! instead of an empty page, and a record given up while still unread is logged, one line
//! per subject and count. Records of one subject are given up oldest first (bytes and age
//! take the oldest, and a group that read a record read every older one of its filter), so a
//! subject's floor never passes a record of it the store still holds. `oldest` is that
//! floor for a filter: the base for what the prefix rule and the last-value rules can match,
//! the matching subjects' floors (and where the rules took effect) for the rest.
//!
//! # Record times
//!
//! The log's records carry no time. The retention tick STAMPS what it has not seen yet with
//! its own clock (`stamp`): a record's age is measured from the first tick that saw it, so it
//! reads at most one tick younger than it is, never older. The stamps are DURABLE: each job
//! writes the ones its tick took into the held store (`held.rs`, its `S` frames), so a
//! reopen reads every record's age back, and an age rule cuts by when a record was first
//! seen whatever the restarts between (a record no tick saw before a restart is stamped at
//! the first tick after it, a restart's downtime younger). A crash that loses records a tick
//! stamped cuts the stamps back to the log's head at the open (`clamp_stamps`), so a record
//! that takes a lost one's offset is never read as old as it. Held records keep their own
//! stamp in the held store. Every age decision is the tick's, taken at the time it is handed
//! (`Broker::retention_tick_at`), so a test drives it with a hand clock and nothing reads
//! the wall clock here.

use crate::brecord::BrokerRecord;
use crate::store::is_hidden_subject;
use astream_wire::{Filter, Subject};
use std::collections::{BTreeMap, HashMap};
use std::io;
use std::sync::Arc;
use std::time::Duration;

/// The most rules a broker takes (the default rule apart).
pub const MAX_RULES: usize = 32;

/// The most classes a broker's rules make: one per rule and the default one.
const MAX_CLASSES: usize = MAX_RULES + 1;

/// The most reader templates one rule names ([`Rule::until_read`]).
pub const MAX_READERS: usize = 8;

/// The highest segment a reader template can name (`{N}`).
const MAX_TEMPLATE_SEGMENT: usize = 64;

/// A class past its bytes gives up at least `max_bytes / SLACK_DIV` of them (see the module
/// docs, "Every class bounded").
pub const SLACK_DIV: u64 = 4;

/// The least a class past `max` bytes gives up.
const fn slack(max: u64) -> u64 {
    max / SLACK_DIV
}

/// The most per-subject floors one [`oldest`] reads before it answers the greatest of all
/// of them instead: a bound on the work a `Fetch` does under the log lock, never a
/// guess below a floor (the fallback is larger, never smaller).
pub(crate) const FLOOR_SCAN_MAX: usize = 65_536;

/// A rule's class (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Class {
    /// The one rule that holds the cut: its records below it are gone.
    Prefix,
    /// A rule whose still-wanted records below the cut are carried in the held store.
    Hold,
}

/// What a hold rule wants below the cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Keep {
    /// Every record within its `max_age` its readers have not read.
    All,
    /// Each subject's `Last` row only (which every compaction carries in `<log>.state`).
    Last,
}

/// One retention rule: `(filter, class, max_age, max_bytes)`, what a hold rule keeps, and the
/// consumer groups that read its records. Built with [`Rule::prefix`], [`Rule::hold`] or
/// [`Rule::default_hold`] and the bounds after them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    filter: Option<Filter>,
    class: Class,
    keep: Keep,
    max_age_ms: Option<u64>,
    max_bytes: Option<u64>,
    read_by: Vec<String>,
}

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg)
}

/// A duration in whole milliseconds, saturating.
fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

impl Rule {
    fn with(filter: &str, class: Class) -> io::Result<Rule> {
        let Ok(f) = Filter::new(filter) else {
            return Err(invalid(format!("retention: {filter:?} is not a filter")));
        };
        Ok(Rule {
            filter: Some(f),
            class,
            keep: Keep::All,
            max_age_ms: None,
            max_bytes: None,
            read_by: Vec::new(),
        })
    }

    /// The PREFIX rule over `filter`: the one rule that holds the cut.
    pub fn prefix(filter: &str) -> io::Result<Rule> {
        Rule::with(filter, Class::Prefix)
    }

    /// A HOLD rule over `filter`, keeping every record ([`Keep::All`]) until its bounds.
    pub fn hold(filter: &str) -> io::Result<Rule> {
        Rule::with(filter, Class::Hold)
    }

    /// The DEFAULT rule as a broker takes it unless told otherwise: a hold rule over every
    /// subject no other rule names that keeps each subject's `Last` row only, with no bounds.
    #[must_use]
    pub const fn default_hold() -> Rule {
        Rule {
            filter: None,
            class: Class::Hold,
            keep: Keep::Last,
            max_age_ms: None,
            max_bytes: None,
            read_by: Vec::new(),
        }
    }

    /// The default rule keeping every record ([`Keep::All`]) until its bounds.
    #[must_use]
    pub const fn default_keeping_all() -> Rule {
        Rule {
            filter: None,
            class: Class::Hold,
            keep: Keep::All,
            max_age_ms: None,
            max_bytes: None,
            read_by: Vec::new(),
        }
    }

    /// No record older than `age` is wanted.
    #[must_use]
    pub fn max_age(mut self, age: Duration) -> Rule {
        self.max_age_ms = Some(millis(age));
        self
    }

    /// The class's records, retained and held, stay within `bytes` ([`record_bytes`]).
    #[must_use]
    pub const fn max_bytes(mut self, bytes: u64) -> Rule {
        self.max_bytes = Some(bytes);
        self
    }

    /// A hold rule that keeps each subject's `Last` row only.
    #[must_use]
    pub const fn last_only(mut self) -> Rule {
        self.keep = Keep::Last;
        self
    }

    /// A record of this rule is READ once the consumer group `template` names has committed
    /// past it; a record every named group has read is wanted no more. `{N}` in the template
    /// is the record subject's `N`-th segment, from 1: in a fabric,
    /// `/f/{2}/cur/{4}/node/inbox` is the inbox group of the node `/f/<F>/in/<node>/…` names.
    /// A record whose subject has too few segments for a template has no group there, and is
    /// held for its `max_age` alone. Refused for a template that does not make a subject.
    pub fn until_read(mut self, template: &str) -> io::Result<Rule> {
        if self.read_by.len() >= MAX_READERS {
            return Err(invalid(format!(
                "retention: a rule names at most {MAX_READERS} reader groups"
            )));
        }
        let Some(probe) = expand_with(template, |_| Some("x")) else {
            return Err(invalid(format!(
                "retention: the reader template {template:?} has a placeholder that is not {{1}} to {{{MAX_TEMPLATE_SEGMENT}}}"
            )));
        };
        if Subject::new(probe).is_err() {
            return Err(invalid(format!(
                "retention: the reader template {template:?} does not make a subject"
            )));
        }
        self.read_by.push(template.to_string());
        Ok(self)
    }

    /// The rule's filter; `None` for the default rule.
    #[must_use]
    pub fn filter(&self) -> Option<&str> {
        match &self.filter {
            Some(f) => Some(f.as_str()),
            None => None,
        }
    }

    /// The rule's class.
    #[must_use]
    pub const fn class(&self) -> Class {
        self.class
    }

    /// What the rule keeps below the cut.
    #[must_use]
    pub const fn keep(&self) -> Keep {
        self.keep
    }

    /// The age past which no record is wanted.
    #[must_use]
    pub const fn age(&self) -> Option<Duration> {
        match self.max_age_ms {
            Some(ms) => Some(Duration::from_millis(ms)),
            None => None,
        }
    }

    /// The bytes the class stays within.
    #[must_use]
    pub const fn bytes(&self) -> Option<u64> {
        self.max_bytes
    }

    /// The reader group templates ([`until_read`](Self::until_read)).
    #[must_use]
    pub fn readers(&self) -> &[String] {
        &self.read_by
    }

    /// Whether the rule keeps every record below the cut in the held store.
    const fn holds_records(&self) -> bool {
        matches!((self.class, self.keep), (Class::Hold, Keep::All))
    }

    /// Whether `rec`, at `age_ms`, is past this rule's age.
    fn aged(&self, age_ms: u64) -> bool {
        match self.max_age_ms {
            Some(max) => age_ms > max,
            None => false,
        }
    }

    /// Whether every reader group this rule names for `rec` has committed past it.
    /// `false` with no reader that applies: such a record is held for its age alone.
    fn read(&self, rec: &BrokerRecord, groups: &HashMap<String, u64>) -> bool {
        let mut any = false;
        for t in &self.read_by {
            let Some(group) = expand(t, &rec.subject) else {
                continue;
            };
            any = true;
            match groups.get(&group) {
                Some(&upto) if upto >= rec.seq.0 => {}
                _ => return false,
            }
        }
        any
    }
}

/// `template` with each `{N}` replaced by `seg(N)`; `None` for a placeholder that is not
/// `{1}` to `{MAX_TEMPLATE_SEGMENT}`, or that `seg` has nothing for.
fn expand_with<'a>(template: &str, seg: impl Fn(usize) -> Option<&'a str>) -> Option<String> {
    let mut out = String::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(rest.get(..open)?);
        let after = rest.get(open.saturating_add(1)..)?;
        let close = after.find('}')?;
        let n: usize = after.get(..close)?.parse().ok()?;
        if n == 0 || n > MAX_TEMPLATE_SEGMENT {
            return None;
        }
        out.push_str(seg(n)?);
        rest = after.get(close.saturating_add(1)..)?;
    }
    if rest.contains('}') {
        return None;
    }
    out.push_str(rest);
    Some(out)
}

/// The group `template` names for `subject` (see [`Rule::until_read`]).
fn expand(template: &str, subject: &str) -> Option<String> {
    let segs: Vec<&str> = subject
        .split('/')
        .skip(1)
        .take(MAX_TEMPLATE_SEGMENT)
        .collect();
    expand_with(template, |n| match n.checked_sub(1) {
        Some(i) => segs.get(i).copied(),
        None => None,
    })
}

/// Whether some subject can match both filters. SOUND for the use it has: it never answers
/// `false` for two filters that share a subject (where a segment could go either way it
/// says `true`), so a floor it is asked about is never missed, only, at worst, answered for
/// a filter that could not have reached it.
pub(crate) fn filters_overlap(a: &str, b: &str) -> bool {
    let mut x = a.split('/').skip(1);
    let mut y = b.split('/').skip(1);
    loop {
        match (x.next(), y.next()) {
            (None, None) => return true,
            // `>` takes one or more segments: anything left on the other side, or another `>`.
            (Some(">"), Some(_)) | (Some(_), Some(">")) => return true,
            (Some(_), None) | (None, Some(_)) => return false,
            (Some(s), Some(t)) => {
                if s != "*" && t != "*" && s != t {
                    return false;
                }
            }
        }
    }
}

/// A record's bytes as retention counts them: its frame's length on a plaintext log (the
/// frame header, the record's fields, its subject and body, a commit it carries), without
/// the batch mark. A sealed log stores a fixed overhead more per record.
#[must_use]
pub fn record_bytes(rec: &BrokerRecord) -> u64 {
    let commit = match &rec.commit {
        Some(c) => c.group.len().saturating_add(12),
        None => 0,
    };
    let n = astream_wire::HEADER_SIZE
        .saturating_add(crate::brecord::RECORD_FIXED_LEN)
        .saturating_add(rec.subject.len())
        .saturating_add(rec.body.len())
        .saturating_add(commit);
    n as u64
}

/// A broker's retention rules: every [`Rule`] in order, then the default one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retention {
    rules: Vec<Rule>,
    default: Rule,
}

impl Retention {
    /// The rules, checked: at most [`MAX_RULES`], each with a filter, exactly one of them
    /// [`Class::Prefix`] (keeping all, naming no reader), and `default` with no filter and
    /// [`Class::Hold`]. What Hello advertises must fit its entry.
    pub fn new(rules: Vec<Rule>, default: Rule) -> io::Result<Retention> {
        if rules.len() > MAX_RULES {
            return Err(invalid(format!(
                "retention: {} rules, at most {MAX_RULES}",
                rules.len()
            )));
        }
        let mut prefixes = 0usize;
        for r in &rules {
            let Some(f) = r.filter() else {
                return Err(invalid(
                    "retention: only the default rule has no filter".to_string(),
                ));
            };
            if r.class == Class::Prefix {
                prefixes = prefixes.saturating_add(1);
                if r.keep != Keep::All || !r.read_by.is_empty() {
                    return Err(invalid(format!(
                        "retention: the prefix rule {f:?} keeps every record it wants and names no reader"
                    )));
                }
            }
        }
        if prefixes != 1 {
            return Err(invalid(format!(
                "retention: exactly one prefix rule holds the cut, not {prefixes}"
            )));
        }
        if default.filter.is_some() || default.class != Class::Hold {
            return Err(invalid(
                "retention: the default rule is a hold rule with no filter".to_string(),
            ));
        }
        let r = Retention { rules, default };
        if r.encode().len() > usize::from(u16::MAX) {
            return Err(invalid(
                "retention: the rules do not fit Hello's entry".to_string(),
            ));
        }
        Ok(r)
    }

    /// The rules, without the default one.
    #[must_use]
    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    /// The default rule.
    #[must_use]
    pub const fn default_rule(&self) -> &Rule {
        &self.default
    }

    /// How many classes there are: one per rule and the default one, last. At most
    /// [`MAX_RULES`] and one ([`Retention::new`] checks it; bounded here too, so every
    /// allocation sized by it is visibly bounded where it is made).
    pub(crate) fn classes(&self) -> usize {
        self.rules
            .len()
            .saturating_add(1)
            .min(MAX_RULES.saturating_add(1))
    }

    /// One `zero` per class. The count is checked against [`MAX_CLASSES`] here, beside the
    /// allocation, so the verified lane sees the bound where the vector is made: a plain
    /// `if` that dominates it (a match guard's bound did not reach the lane's solver).
    /// [`classes`](Self::classes) never exceeds it, so the empty answer is never given.
    pub(crate) fn per_class<T: Clone>(&self, zero: T) -> Vec<T> {
        let n = self.classes();
        if n > MAX_CLASSES {
            return Vec::new();
        }
        vec![zero; n]
    }

    /// The rule of class `c` (the default rule for `c` past the rules).
    pub(crate) fn rule(&self, c: usize) -> &Rule {
        match self.rules.get(c) {
            Some(r) => r,
            None => &self.default,
        }
    }

    /// The class of `subject`: the first rule whose filter matches it, else the default one,
    /// which every hidden subject is.
    pub(crate) fn class_of(&self, subject: &str) -> usize {
        if !is_hidden_subject(subject) {
            for (i, r) in self.rules.iter().enumerate() {
                if let Some(f) = &r.filter {
                    if f.matches_str(subject) {
                        return i;
                    }
                }
            }
        }
        self.rules.len()
    }

    /// The classes `filter` can reach: every rule whose filter can share a subject with it,
    /// and the default one unless a single rule's filter holds every subject it matches.
    pub(crate) fn reached_by(&self, filter: &Filter) -> Vec<usize> {
        let mut out = Vec::new();
        let mut covered = false;
        for (i, r) in self.rules.iter().enumerate() {
            if let Some(f) = &r.filter {
                if filters_overlap(f.as_str(), filter.as_str()) {
                    out.push(i);
                }
                if f.contains(filter) {
                    covered = true;
                }
            }
        }
        if !covered {
            out.push(self.rules.len());
        }
        out
    }

    /// The classes a subject of ANOTHER rules' default class can be in here: every rule
    /// whose filter is none of `named` (theirs), and the default one. A subject no rule of
    /// theirs names matches none of `named`, so none of the rules here with those filters.
    pub(crate) fn reached_by_default_of(&self, named: &[&str]) -> Vec<usize> {
        let mut out = Vec::new();
        for (i, r) in self.rules.iter().enumerate() {
            match r.filter() {
                Some(f) if named.contains(&f) => {}
                _ => out.push(i),
            }
        }
        out.push(self.rules.len());
        out
    }

    /// Hello's entry for the rules (`proto` tag 0x05): `n u8`, then per rule, the default one
    /// last, `class u8, keep u8, max_age_ms u64 LE, max_bytes u64 LE` (`u64::MAX`: none),
    /// `filter len u16 LE` and its bytes (none for the default rule), `readers u8` and each
    /// `len u16 LE` and its bytes.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let n = u8::try_from(self.classes()).unwrap_or(u8::MAX);
        out.push(n);
        for r in self.rules.iter().chain(std::iter::once(&self.default)) {
            put_rule(&mut out, r);
        }
        out
    }

    /// The rules Hello's entry advertises, the default one last; `None` for bytes that are
    /// not that layout or rules that do not check ([`Retention::new`]).
    #[must_use]
    pub fn decode(b: &[u8]) -> Option<Retention> {
        let (&n, mut rest) = b.split_first()?;
        let mut rules = Vec::new();
        for _ in 0..n {
            let (r, tail) = take_rule(rest)?;
            rules.push(r);
            rest = tail;
        }
        if !rest.is_empty() {
            return None;
        }
        let default = rules.pop()?;
        Retention::new(rules, default).ok()
    }
}

fn put_u16_str(out: &mut Vec<u8>, s: &str) {
    let len = u16::try_from(s.len()).unwrap_or(u16::MAX);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(s.as_bytes().get(..usize::from(len)).unwrap_or(&[]));
}

fn put_rule(out: &mut Vec<u8>, r: &Rule) {
    out.push(match r.class {
        Class::Prefix => 0,
        Class::Hold => 1,
    });
    out.push(match r.keep {
        Keep::All => 0,
        Keep::Last => 1,
    });
    out.extend_from_slice(&r.max_age_ms.unwrap_or(u64::MAX).to_le_bytes());
    out.extend_from_slice(&r.max_bytes.unwrap_or(u64::MAX).to_le_bytes());
    put_u16_str(out, r.filter().unwrap_or(""));
    out.push(u8::try_from(r.read_by.len()).unwrap_or(u8::MAX));
    for t in &r.read_by {
        put_u16_str(out, t);
    }
}

fn take_u64(b: &[u8]) -> Option<(u64, &[u8])> {
    let (head, rest) = b.split_first_chunk::<8>()?;
    Some((u64::from_le_bytes(*head), rest))
}

fn take_u16_str(b: &[u8]) -> Option<(&str, &[u8])> {
    let (len, rest) = b.split_first_chunk::<2>()?;
    let len = usize::from(u16::from_le_bytes(*len));
    let s = rest.get(..len)?;
    Some((std::str::from_utf8(s).ok()?, rest.get(len..)?))
}

fn take_rule(b: &[u8]) -> Option<(Rule, &[u8])> {
    let (&class, rest) = b.split_first()?;
    let (&keep, rest) = rest.split_first()?;
    let (age, rest) = take_u64(rest)?;
    let (bytes, rest) = take_u64(rest)?;
    let (filter, rest) = take_u16_str(rest)?;
    let (&readers, mut rest) = rest.split_first()?;
    let class = match class {
        0 => Class::Prefix,
        1 => Class::Hold,
        _ => return None,
    };
    let mut r = if filter.is_empty() {
        Rule::default_keeping_all()
    } else {
        Rule::with(filter, class).ok()?
    };
    r.class = class;
    r.keep = match keep {
        0 => Keep::All,
        1 => Keep::Last,
        _ => return None,
    };
    r.max_age_ms = if age == u64::MAX { None } else { Some(age) };
    r.max_bytes = if bytes == u64::MAX { None } else { Some(bytes) };
    for _ in 0..readers {
        let (t, tail) = take_u16_str(rest)?;
        r = r.until_read(t).ok()?;
        rest = tail;
    }
    Some((r, rest))
}

/// A record the held store carries, and the time the tick first saw it.
#[derive(Debug, Clone)]
pub(crate) struct Held {
    pub(crate) rec: Arc<BrokerRecord>,
    pub(crate) at_ms: u64,
}

/// The tick's stamps: `(head, ms)`, heads rising: every record below `head` was first seen
/// by the tick at `ms` or earlier.
pub(crate) type Stamps = Vec<(u64, u64)>;

/// The most stamps a log keeps: past it, two neighbours merge into the later one, so a
/// record never reads older than it is.
pub(crate) const STAMPS_MAX: usize = 4096;

/// Stamp every record below `head` the tick has not seen yet with `now` (never before the
/// last stamp), and keep at most [`STAMPS_MAX`].
pub(crate) fn stamp(stamps: &mut Stamps, head: u64, now: u64) {
    let (last_head, last_ms) = match stamps.last() {
        Some(&s) => s,
        None => (0, 0),
    };
    if head > last_head || stamps.is_empty() {
        stamps.push((head, now.max(last_ms)));
    }
    if stamps.len() > STAMPS_MAX {
        let mut merged = Vec::with_capacity(STAMPS_MAX / 2 + 1);
        let mut it = stamps.chunks(2);
        for pair in &mut it {
            if let Some(&later) = pair.last() {
                merged.push(later);
            }
        }
        *stamps = merged;
    }
}

/// Drop the stamps no retained record needs: every one but the last below `base`.
pub(crate) fn prune_stamps(stamps: &mut Stamps, base: u64) {
    let below = stamps.iter().take_while(|&&(h, _)| h <= base).count();
    if below > 0 {
        // A copy of what stays, not `drain`, whose panic on a range past the length the
        // verified lane does not model.
        *stamps = stamps.get(below..).unwrap_or(&[]).to_vec();
    }
}

/// Cut the stamps back to `head`: every record below it keeps the stamp it had, and none at
/// or above it has one (a stamp past `head` covers only the records below it from here). An
/// open whose log ends below its stamps cuts them so, as the held store's `S` frame replays.
pub(crate) fn clamp_stamps(stamps: &mut Stamps, head: u64) {
    let keep = stamps.partition_point(|&(h, _)| h <= head);
    let below = match keep.checked_sub(1) {
        Some(i) => match stamps.get(i) {
            Some(&(h, _)) => h,
            None => 0,
        },
        None => 0,
    };
    let straddle = match stamps.get(keep) {
        Some(&(_, ms)) if below < head => Some(ms),
        _ => None,
    };
    if keep < stamps.len() {
        // A copy of what stays, as `prune_stamps` takes it.
        *stamps = stamps.get(..keep).unwrap_or(&[]).to_vec();
    }
    if let Some(ms) = straddle {
        stamps.push((head, ms));
    }
}

/// When the tick first saw the record at `off`: the first stamp above it, or `now` for one
/// no tick has stamped yet.
pub(crate) fn stamped_at(stamps: &[(u64, u64)], off: u64, now: u64) -> u64 {
    let i = stamps.partition_point(|&(h, _)| h <= off);
    match stamps.get(i) {
        Some(&(_, ms)) => ms,
        None => now,
    }
}

/// What the planning reads: the log as one moment saw it.
pub(crate) struct View<'a> {
    pub(crate) base: u64,
    /// The cut never rises above it: the subscriber-visible head (and, replicated, what every
    /// follower holds).
    pub(crate) ceiling: u64,
    /// The retained records, from the base, in offset order.
    pub(crate) records: &'a [Arc<BrokerRecord>],
    pub(crate) stamps: &'a [(u64, u64)],
    /// The groups' durable commits (`upto`, inclusive).
    pub(crate) groups: &'a HashMap<String, u64>,
    /// The held store, by offset (every one below the base).
    pub(crate) held: &'a BTreeMap<u64, Held>,
    pub(crate) now: u64,
}

/// Why a hold rule gave a record up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Why {
    /// Every reader it names read it.
    Read,
    /// Past the rule's age.
    Aged,
    /// Past the class's bytes.
    Bytes,
}

/// What one planning decided: the cut, the records newly held, the held ones given up, and
/// the floors that rise.
#[derive(Debug, Default)]
pub(crate) struct Plan {
    pub(crate) cut: u64,
    /// Records below the cut, at or above the base, to carry: each with its stamp.
    pub(crate) carry: Vec<Held>,
    /// Held offsets (below the base) given up.
    pub(crate) remove: Vec<u64>,
    /// Subject floors that rise, to the offset after the newest record of it given up.
    pub(crate) floors: BTreeMap<String, u64>,
    /// Unread records given up, by subject and why: how many, for the log line.
    pub(crate) lost: BTreeMap<(String, Why), u64>,
    /// Each class's bytes in the retained suffix the cut keeps.
    pub(crate) kept: Vec<u64>,
    /// What the planning leaves each class before it is past its own bytes, retained above
    /// the cut and held both: the class can take this many more bytes of its own records
    /// before it asks for a tick. `u64::MAX` for a class that bounds no bytes; `0` for one
    /// still past them (a caller's cut below the rules'); a quarter of its bytes for one
    /// the cut could not bring within them because the visible head held it back, so it
    /// does not ask again for every record while that lasts.
    pub(crate) room: Vec<u64>,
    /// The least of [`room`](Self::room): `u64::MAX` with no class that bounds bytes.
    pub(crate) headroom: u64,
}

impl Plan {
    /// Whether the plan changes nothing.
    pub(crate) fn is_empty(&self, base: u64) -> bool {
        self.cut <= base
            && self.carry.is_empty()
            && self.remove.is_empty()
            && self.floors.is_empty()
    }

    fn give_up(&mut self, rec: &BrokerRecord, why: Why) {
        let floor = rec.seq.0.saturating_add(1);
        let f = self.floors.entry(rec.subject.clone()).or_insert(0);
        *f = (*f).max(floor);
        if why != Why::Read {
            let n = self.lost.entry((rec.subject.clone(), why)).or_insert(0);
            *n = n.saturating_add(1);
        }
    }
}

/// What one walk of the retained records finds: each class's bytes in them, and the
/// offset just above the newest prefix record past its rule's age (the base for none).
struct Walk {
    bytes: Vec<u64>,
    aged_to: u64,
}

fn walk(rules: &Retention, v: &View<'_>) -> Walk {
    let mut bytes = rules.per_class(0u64);
    let mut aged_to = v.base;
    for rec in v.records {
        let c = rules.class_of(&rec.subject);
        if let Some(slot) = bytes.get_mut(c) {
            *slot = slot.saturating_add(record_bytes(rec));
        }
        let rule = rules.rule(c);
        if rule.class == Class::Prefix {
            let age = v.now.saturating_sub(stamped_at(v.stamps, rec.seq.0, v.now));
            if rule.aged(age) {
                aged_to = aged_to.max(rec.seq.0.saturating_add(1));
            }
        }
    }
    Walk { bytes, aged_to }
}

/// What each class may keep after the planning: its `max_bytes`, or, for a class past them
/// (`has`, retained and still-wanted held), the lower of them and what it has less a
/// [`slack`] (`u64::MAX` for a class that bounds no bytes).
fn limits(rules: &Retention, retained: &[u64], held: &[u64]) -> Vec<u64> {
    let mut out = rules.per_class(u64::MAX);
    for (c, slot) in out.iter_mut().enumerate() {
        let Some(max) = rules.rule(c).max_bytes else {
            continue;
        };
        let has = match (retained.get(c), held.get(c)) {
            (Some(&r), Some(&h)) => r.saturating_add(h),
            (Some(&r), None) => r,
            (None, Some(&h)) => h,
            (None, None) => 0,
        };
        *slot = if has > max {
            max.min(has.saturating_sub(slack(max)))
        } else {
            max
        };
    }
    out
}

/// THE CUT the rules take on `v`: walking up from the base, each class's oldest retained
/// records go below the cut until what the class keeps above it is within its `limit`, and
/// every prefix record past its age is below it. Not yet held to the ceiling.
fn natural_cut(rules: &Retention, v: &View<'_>, w: &Walk, limit: &[u64]) -> u64 {
    let mut need = rules.per_class(0u64);
    let mut left = 0usize;
    for (c, n) in need.iter_mut().enumerate() {
        let has = w.bytes.get(c).copied().unwrap_or(0);
        let lim = limit.get(c).copied().unwrap_or(u64::MAX);
        *n = has.saturating_sub(lim);
        if *n > 0 {
            left = left.saturating_add(1);
        }
    }
    let mut cut = v.base;
    for rec in v.records {
        if left == 0 && rec.seq.0 >= w.aged_to {
            break;
        }
        let c = rules.class_of(&rec.subject);
        if let Some(n) = need.get_mut(c) {
            if *n > 0 {
                *n = n.saturating_sub(record_bytes(rec));
                if *n == 0 {
                    left = left.saturating_sub(1);
                }
            }
        }
        cut = rec.seq.0.saturating_add(1);
    }
    cut.max(w.aged_to)
}

/// THE PLANNING: the cut (`forced`, when the caller names one, else [`natural_cut`]), and
/// what the hold rules do about it — carry each record below it that a [`Keep::All`] hold
/// rule still wants, give up the held records read or aged since, and keep each class
/// within its limit (its bytes, at least a [`slack`] under what it had when it was past
/// them) by giving up its oldest, retained above the cut counted first — and the room each
/// class has left.
pub(crate) fn plan(rules: &Retention, v: &View<'_>, forced: Option<u64>) -> Plan {
    let mut p = Plan {
        cut: v.base,
        headroom: u64::MAX,
        ..Plan::default()
    };
    // The class's held candidates, oldest first: (offset, bytes, from the store or new).
    let mut by_class: Vec<Vec<(u64, u64, bool)>> = rules.per_class(Vec::new());
    let mut held = rules.per_class(0u64);
    for (&off, h) in v.held {
        let c = rules.class_of(&h.rec.subject);
        let rule = rules.rule(c);
        // A record its rule no longer holds (the rules changed) goes as a read one does.
        let why = if !rule.holds_records() || rule.read(&h.rec, v.groups) {
            Some(Why::Read)
        } else if rule.aged(v.now.saturating_sub(h.at_ms)) {
            Some(Why::Aged)
        } else {
            None
        };
        match why {
            Some(w) => {
                p.remove.push(off);
                p.give_up(&h.rec, w);
            }
            None => {
                let b = record_bytes(&h.rec);
                if let Some(list) = by_class.get_mut(c) {
                    list.push((off, b, false));
                }
                if let Some(slot) = held.get_mut(c) {
                    *slot = slot.saturating_add(b);
                }
            }
        }
    }
    let w = walk(rules, v);
    let limit = limits(rules, &w.bytes, &held);
    let want = match forced {
        Some(c) => c,
        None => natural_cut(rules, v, &w, &limit),
    };
    let cut = want.min(v.ceiling).max(v.base);
    // The visible head held the cut back: a class may still be past its bytes.
    let capped = want > cut;
    p.cut = cut;
    let kept = kept_bytes(rules, v, cut);
    let below = usize::try_from(cut.saturating_sub(v.base)).unwrap_or(usize::MAX);
    let mut fresh: BTreeMap<u64, Held> = BTreeMap::new();
    for rec in v.records.iter().take(below) {
        if is_hidden_subject(&rec.subject) {
            continue;
        }
        let c = rules.class_of(&rec.subject);
        let rule = rules.rule(c);
        if !rule.holds_records() {
            continue;
        }
        let at_ms = stamped_at(v.stamps, rec.seq.0, v.now);
        if rule.read(rec, v.groups) {
            p.give_up(rec, Why::Read);
        } else if rule.aged(v.now.saturating_sub(at_ms)) {
            p.give_up(rec, Why::Aged);
        } else {
            if let Some(list) = by_class.get_mut(c) {
                list.push((rec.seq.0, record_bytes(rec), true));
            }
            fresh.insert(
                rec.seq.0,
                Held {
                    rec: Arc::clone(rec),
                    at_ms,
                },
            );
        }
    }
    // Each class within its limit: what it keeps above the cut first, then its held
    // records, the newest kept and the oldest given up.
    let mut room = rules.per_class(u64::MAX);
    for (c, list) in by_class.iter().enumerate() {
        let rule = rules.rule(c);
        let Some(max) = rule.max_bytes else {
            continue;
        };
        let above = kept.get(c).copied().unwrap_or(0);
        let budget = limit.get(c).copied().unwrap_or(max).saturating_sub(above);
        let mut total: u64 = 0;
        for &(_, b, _) in list {
            total = total.saturating_add(b);
        }
        for &(off, b, is_new) in list {
            if total <= budget {
                break;
            }
            total = total.saturating_sub(b);
            let rec = if is_new {
                fresh.remove(&off).map(|h| h.rec)
            } else {
                p.remove.push(off);
                v.held.get(&off).map(|h| Arc::clone(&h.rec))
            };
            if let Some(rec) = rec {
                p.give_up(&rec, Why::Bytes);
            }
        }
        let used = above.saturating_add(total);
        let left = if used <= max {
            max.saturating_sub(used)
        } else if capped {
            slack(max)
        } else {
            0
        };
        if let Some(slot) = room.get_mut(c) {
            *slot = left;
        }
        p.headroom = p.headroom.min(left);
    }
    p.kept = kept;
    p.room = room;
    p.carry = fresh.into_values().collect();
    p.remove.sort_unstable();
    p.remove.dedup();
    p
}

/// Each class's bytes in the retained suffix from `cut`.
fn kept_bytes(rules: &Retention, v: &View<'_>, cut: u64) -> Vec<u64> {
    let mut acc = rules.per_class(0u64);
    let skip = usize::try_from(cut.saturating_sub(v.base)).unwrap_or(usize::MAX);
    for rec in v.records.iter().skip(skip) {
        let c = rules.class_of(&rec.subject);
        if let Some(slot) = acc.get_mut(c) {
            *slot = slot.saturating_add(record_bytes(rec));
        }
    }
    acc
}

/// What a read of `filter` cannot reach below: the OLDEST offset from which every record of
/// it the rules still want is readable. The base for what the prefix rule and the last-value
/// rules can match (and for everything, with no rules); for what a [`Keep::All`] hold rule
/// can match, from where its class is whole (`since`, by class: the base for a class it
/// does not name), the `spill` (the floors folded
/// together to keep them bounded) and the floor of every matching subject that gave records
/// up. Past [`FLOOR_SCAN_MAX`] subjects it answers the greatest of all floors (`floor_max`)
/// instead: larger, never smaller.
pub(crate) fn oldest(
    rules: Option<&Retention>,
    base: u64,
    since: &[u64],
    floors: &BTreeMap<String, u64>,
    spill: u64,
    floor_max: u64,
    filter: &Filter,
) -> u64 {
    let Some(rules) = rules else {
        return base;
    };
    let mut o = 0u64;
    let mut holds = false;
    for c in rules.reached_by(filter) {
        if rules.rule(c).holds_records() {
            holds = true;
            o = o.max(since.get(c).copied().unwrap_or(base).min(base));
        } else {
            o = o.max(base);
        }
    }
    if !holds {
        return o;
    }
    o = o.max(spill.min(base));
    let prefix = crate::store::literal_prefix(filter);
    let mut seen = 0usize;
    let range = floors.range::<str, _>((
        std::ops::Bound::Included(prefix),
        std::ops::Bound::Unbounded,
    ));
    for (subject, &f) in range {
        if !subject.starts_with(prefix) {
            break;
        }
        seen = seen.saturating_add(1);
        if seen > FLOOR_SCAN_MAX {
            return o.max(floor_max.min(base));
        }
        if filter.matches_str(subject) {
            o = o.max(f.min(base));
        }
    }
    o
}

/// FROM WHERE EACH CLASS OF `new` IS WHOLE when it replaces `old` at the base `base`, `old`
/// whole from `old_since` class by class: a class that keeps records ([`Keep::All`] hold)
/// takes the latest of where the old classes its subjects can have been in became whole —
/// the old classes whose filter can share a subject with its own ([`Retention::reached_by`]),
/// or, for the default class, every old class whose filter no new rule names and the old
/// default — an old class that kept none of them below the base (the prefix, a last-value
/// rule) counting as the base, since its records there were dropped, not carried. Every
/// other class, and every class without old rules, starts at the base. SOUND: a class is
/// never answered whole from below where some subject of it was last kept whole, so a
/// rules change that only retunes a class's bounds keeps its held records readable, and a
/// class new to the rules is told where its records start.
pub(crate) fn carried_since(
    old: Option<&Retention>,
    old_since: &[u64],
    new: &Retention,
    base: u64,
) -> Vec<u64> {
    let mut out = new.per_class(base);
    let Some(old) = old else {
        return out;
    };
    let named: Vec<&str> = new.rules.iter().filter_map(Rule::filter).collect();
    for (c, slot) in out.iter_mut().enumerate() {
        if !new.rule(c).holds_records() {
            continue;
        }
        let from = match new.rules.get(c).and_then(|r| r.filter.as_ref()) {
            Some(f) => old.reached_by(f),
            None => old.reached_by_default_of(&named),
        };
        let mut since = 0u64;
        for o in from {
            let s = if old.rule(o).holds_records() {
                old_since.get(o).copied().unwrap_or(base)
            } else {
                base
            };
            since = since.max(s);
        }
        *slot = since.min(base);
    }
    out
}

/// One rule as a line of a rules file (`asb serve --retain-file`): `<prefix|hold|default>`,
/// then its options, then its filter (none for `default`), split on spaces. The options are
/// `age=<n><ms|s|m|h|d>`, `bytes=<n>[KiB|MiB|GiB]`, `last` and `read=<template>`. A filter
/// with a space in it cannot be written here. Returns whether the line is the default rule.
pub fn parse_rule_line(line: &str) -> Result<(bool, Rule), String> {
    let mut words = line.split_ascii_whitespace();
    let Some(head) = words.next() else {
        return Err("an empty rule".to_string());
    };
    let rest: Vec<&str> = words.collect();
    let (opts, filter) = match head {
        "default" => (&rest[..], None),
        "prefix" | "hold" => match rest.split_last() {
            Some((f, opts)) => (opts, Some(*f)),
            None => return Err(format!("{head}: no filter")),
        },
        other => return Err(format!("{other:?}: expected prefix, hold or default")),
    };
    let mut rule = match (head, filter) {
        ("prefix", Some(f)) => Rule::prefix(f).map_err(|e| e.to_string())?,
        ("hold", Some(f)) => Rule::hold(f).map_err(|e| e.to_string())?,
        _ => Rule::default_keeping_all(),
    };
    for opt in opts {
        if *opt == "last" {
            rule = rule.last_only();
        } else if let Some(v) = opt.strip_prefix("age=") {
            rule = rule.max_age(parse_age(v)?);
        } else if let Some(v) = opt.strip_prefix("bytes=") {
            rule = rule.max_bytes(parse_bytes(v)?);
        } else if let Some(v) = opt.strip_prefix("read=") {
            rule = rule.until_read(v).map_err(|e| e.to_string())?;
        } else {
            return Err(format!("{opt:?}: expected age=, bytes=, last or read="));
        }
    }
    Ok((head == "default", rule))
}

/// A RULES FILE (`asb serve --retain-file`): one [`parse_rule_line`] per line, blank lines
/// and `#` comments skipped, at most one `default` line ([`Rule::default_hold`] without
/// one), checked as [`Retention::new`] checks them. Refused past [`MAX_RULES`] rules as it
/// reads, so a long file costs no more than the rules a broker can take. Each refusal names
/// its line.
pub fn parse_rules(text: &str) -> Result<Retention, String> {
    let mut rules = Vec::new();
    let mut default: Option<Rule> = None;
    for (i, line) in text.lines().enumerate() {
        let n = i.saturating_add(1);
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (is_default, rule) = match parse_rule_line(line) {
            Ok(r) => r,
            Err(e) => return Err(format!("line {n}: {e}")),
        };
        if is_default {
            if default.is_some() {
                return Err(format!("line {n}: a second default rule"));
            }
            default = Some(rule);
        } else {
            if rules.len() >= MAX_RULES {
                return Err(format!("line {n}: more than {MAX_RULES} rules"));
            }
            rules.push(rule);
        }
    }
    let default = match default {
        Some(d) => d,
        None => Rule::default_hold(),
    };
    match Retention::new(rules, default) {
        Ok(r) => Ok(r),
        Err(e) => Err(e.to_string()),
    }
}

fn split_number(v: &str) -> Result<(u64, &str), String> {
    let digits = v.bytes().take_while(u8::is_ascii_digit).count();
    let Some((n, unit)) = v.split_at_checked(digits) else {
        return Err(format!("{v:?}: not a number"));
    };
    match n.parse::<u64>() {
        Ok(n) => Ok((n, unit)),
        Err(_) => Err(format!("{v:?}: not a number")),
    }
}

fn parse_age(v: &str) -> Result<Duration, String> {
    let (n, unit) = split_number(v)?;
    let ms: u64 = match unit {
        "ms" => 1,
        "s" => 1000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        _ => return Err(format!("age={v}: expected ms, s, m, h or d")),
    };
    Ok(Duration::from_millis(n.saturating_mul(ms)))
}

fn parse_bytes(v: &str) -> Result<u64, String> {
    let (n, unit) = split_number(v)?;
    let mul: u64 = match unit {
        "" => 1,
        "KiB" => 1 << 10,
        "MiB" => 1 << 20,
        "GiB" => 1 << 30,
        _ => return Err(format!("bytes={v}: expected KiB, MiB or GiB")),
    };
    Ok(n.saturating_mul(mul))
}

#[cfg(test)]
#[path = "retention_tests.rs"]
mod tests;
