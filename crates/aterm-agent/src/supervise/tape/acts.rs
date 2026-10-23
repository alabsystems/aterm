// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! ACTS MODE: the cursor a COUNTERFACTUAL replay serves the loop from (the replay plan's §5.2) — `aterm drive replay FILE --set k=v…|--policy FILE`, the recorded hosted loop run again under another policy, up to the first act it takes otherwise.

use std::collections::{BTreeMap, BTreeSet};
use std::io;

use astream_effects::EffectRecord;

use super::codec::{self, Stamp};
use super::{ctl, doors};
use crate::supervise::SupervisorConfig;

/// THE CANDIDATE: the policy a counterfactual decides under, made from the recorded one — `--policy FILE`, a whole `[harness]` table read by the owner's own reader in its place, then each `--set k=v` over it through the very [`SupervisorConfig::set`] a header's policy is rebuilt with.
#[derive(Debug, Clone, Default)]
pub struct Candidate {
    /// `--policy FILE`: what was named, and the policy read from it.
    pub file: Option<(String, SupervisorConfig)>,
    /// `--set k=v`, in order (a `harness.` before the key is the table's).
    pub sets: Vec<(String, String)>,
}

impl Candidate {
    /// The candidate's policy, made from the run's `recorded` one.
    /// # Errors
    /// A `--set` the reader refuses — an unknown key, or a value that is not the key's own (an off spelled for a key included: a counterfactual says exactly what it runs, or nothing).
    pub fn policy(&self, recorded: &SupervisorConfig) -> Result<SupervisorConfig, String> {
        let mut policy = match &self.file {
            Some((_, file)) => file.clone(),
            None => recorded.clone(),
        };
        for (key, value) in &self.sets {
            let key = key.strip_prefix("harness.").unwrap_or(key);
            policy
                .set(key, value)
                .map_err(|e| format!("the candidate's {key}={value}: {e}"))?;
        }
        Ok(policy)
    }

    /// What the candidate changes, as a report says it: the file named, then each key set.
    #[must_use]
    pub fn said(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some((named, _)) = &self.file {
            out.push(format!("--policy {named}"));
        }
        out.extend(self.sets.iter().map(|(k, v)| format!("{k}={v}")));
        if out.is_empty() {
            out.push("the recorded policy".to_string());
        }
        out
    }
}

/// What a request is, for acts mode (the replay plan's §5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActClass {
    /// A write into the session ([`ctl::is_act`]: a key, a send, a turn, a paste, a post, an attention write, a task mark, a signal, a restart) or a host's act (`doors::is_host_act`): held to the tape exactly.
    Act,
    /// The claim's renewal and release (`meta set|unset supervisor`), the teller's `story`: the clock's cadence, never compared.
    Housekeeping,
    /// A wait: `await …`.
    Wait,
    /// A round trip whose reply is a write's outcome — a line printed, a notes line, the ledger's bound (`write.*`): an output, never compared.
    Output,
    /// Everything else: a look at the session, the host or the machine.
    Read,
}

impl ActClass {
    /// The class's word, as a report says it.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Act => "act",
            Self::Housekeeping => "housekeeping",
            Self::Wait => "wait",
            Self::Output => "output",
            Self::Read => "read",
        }
    }
}

/// The class of the round trip at `door` asking `request` ([module docs](self)).
#[must_use]
pub fn act_class(door: &str, request: &[u8]) -> ActClass {
    if door == doors::TELLER {
        return ActClass::Housekeeping;
    }
    if door == doors::CTL {
        let Ok(words) = codec::ctl_args(request) else {
            return ActClass::Read;
        };
        let words: Vec<&str> = words
            .iter()
            .map(String::as_str)
            .filter(|w| !w.starts_with('@'))
            .collect();
        if ctl::is_act(&words) {
            return ActClass::Act;
        }
        if ctl::is_wait(&words) {
            return ActClass::Wait;
        }
        return match words.as_slice() {
            ["meta", "set" | "unset", "supervisor", ..] | ["story", ..] => ActClass::Housekeeping,
            _ => ActClass::Read,
        };
    }
    if doors::is_host_act(door) {
        return ActClass::Act;
    }
    if door == doors::OUT || door == doors::NOTES || door.starts_with("write.") {
        return ActClass::Output;
    }
    ActClass::Read
}

/// Whether `record` is an act on the tape.
fn is_act_record(record: &EffectRecord) -> bool {
    matches!(record, EffectRecord::Exchange { door, request, .. }
        if act_class(door, request) == ActClass::Act)
}

/// A wait's kind (the word after `await`: `seq`, `agent`, `gone` …) and its
/// timeout in milliseconds, when it names one.
fn wait_of(request: &[u8]) -> (Option<String>, Option<u64>) {
    let words: Vec<String> = codec::ctl_args(request)
        .unwrap_or_default()
        .into_iter()
        .filter(|w| !w.starts_with('@'))
        .collect();
    let kind = words.get(1).cloned();
    let timeout = words
        .iter()
        .position(|w| w == "timeout")
        .and_then(|at| words.get(at + 1))
        .and_then(|ms| ms.parse().ok());
    (kind, timeout)
}

/// A wait's TARGET: its words, its timeout (the loop's schedule, not what it
/// waits for) left out — `await seq 102`.
fn wait_key(request: &[u8]) -> Vec<String> {
    let words: Vec<String> = codec::ctl_args(request)
        .unwrap_or_default()
        .into_iter()
        .filter(|w| !w.starts_with('@'))
        .collect();
    let mut key = Vec::with_capacity(words.len());
    let mut it = words.into_iter();
    while let Some(w) = it.next() {
        if w == "timeout" {
            it.next();
        } else {
            key.push(w);
        }
    }
    key
}

/// A housekeeping request's KIND: its verb's words, its values left out —
/// `meta set supervisor`, `meta unset supervisor`, `story` — what a request
/// in other words is answered by when its very words are not on the tape.
fn housekeeping_kind(request: &[u8]) -> Vec<String> {
    let words: Vec<String> = codec::ctl_args(request)
        .unwrap_or_default()
        .into_iter()
        .filter(|w| !w.starts_with('@'))
        .collect();
    let verb = match words.first().map(String::as_str) {
        Some("meta") => 3,
        _ => 1,
    };
    words.into_iter().take(verb).collect()
}

/// A control lane's success, saying nothing: what housekeeping no record of
/// its kind answered is answered with.
fn lane_success() -> Vec<u8> {
    codec::ctl_reply(&Ok(crate::supervise::CtlReply {
        code: 0,
        stdout: String::new(),
        stderr: String::new(),
    }))
}

/// What a counterfactual replay did to the tape, counted: the measure of how far the candidate strayed from the recorded run before (or without) taking another act.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActsAccount {
    /// The acts on the tape.
    pub acts: usize,
    /// The acts the candidate took as recorded.
    pub taken: usize,
    /// Records the candidate consumed.
    pub consumed: usize,
    /// Records a resync skipped (each a read, a clock, an output — never an act).
    pub dropped: usize,
    /// Reads served again from earlier in their segment.
    pub repeated: usize,
    /// Derived probes the segment did not hold, answered from before the cursor by astream-effects' keyed rule (the latest recorded answer [`astream_effects::keyed_hit_within_cutoff`] admits at the cursor), read from the cursor's own index.
    pub keyed: usize,
    /// Waits served by the segment's next wait of their kind, with other arguments (schedule taint).
    pub tainted: usize,
    /// Clock reads with no clock record under the cursor: virtual time.
    pub virtual_clock: usize,
    /// Sleeps with no identical sleep under the cursor: virtual time.
    pub slept: usize,
    /// Housekeeping its identical request in the loop's segment did not answer (never compared): served from a segment before the loop's own, by the latest of its kind before the cursor, or as its door's success.
    pub housekept: usize,
    /// Outputs (a line, a row, a mark, a notification) with no record of their door under the cursor, and writes whose outcome no record of their door gave.
    pub unmatched_outputs: usize,
}

impl ActsAccount {
    /// Whether the candidate followed the tape record for record, as a strict replay would: nothing dropped, repeated, keyed, tainted, made up or left unmatched.
    #[must_use]
    pub fn exact(&self) -> bool {
        self.dropped == 0
            && self.repeated == 0
            && self.keyed == 0
            && self.tainted == 0
            && self.virtual_clock == 0
            && self.slept == 0
            && self.housekept == 0
            && self.unmatched_outputs == 0
    }
}

/// Why the cursor would not serve a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// A read (a wait, housekeeping, a file) the loop's segment does not hold and nothing before the cursor may stand for: STALE.
    Stale { class: ActClass },
    /// An act that is not the segment's closing act; `closing` is that act's ordinal, `None` past the tape's last act.
    ActDiffers { closing: Option<usize> },
    /// A wait (of the class `class`) the segment does not hold and the recorded run waited for (its timeout aside) only after the segment's closing act, `closing`: the candidate went on without that act — the first divergent act, the act it skipped.
    Skipped { closing: usize, class: ActClass },
}

/// A request served: the reply, and the ordinal of the record it came from.
pub(crate) struct Served {
    pub(crate) reply: Vec<u8>,
    pub(crate) at: Option<usize>,
}

/// The counterfactual replay's cursor ([module docs](self)).
#[derive(Debug)]
pub(crate) struct ActsCursor {
    records: Vec<EffectRecord>,
    stamps: Vec<Option<Stamp>>,
    /// The ordinals of the tape's acts, ascending.
    acts: Vec<usize>,
    /// Each raised flag's record, by door (astream's door latches: one each).
    raised_at: BTreeMap<String, usize>,
    /// The next record not consumed or dropped.
    pos: usize,
    /// The first record of the loop's segment (just past the last act taken).
    seg_start: usize,
    /// The loop has strayed from the tape in this segment.
    strayed: bool,
    /// The virtual clocks: where the last record consumed stood (monotonic ns
    /// since the tape's origin; wall ns since the epoch), the sleeps served
    /// since, and the last monotonic value served (it never runs back).
    base_mono: u64,
    base_wall: i64,
    slept_mono: u64,
    slept_wall: u64,
    mono_floor: u64,
    /// Flags read true, and each flag's false reads since the last record.
    latched: BTreeSet<String>,
    flag_reads: BTreeMap<String, u64>,
    /// Requests served that consumed no record, in a row (the progress guard).
    pub(crate) idle: u64,
    pub(crate) account: ActsAccount,
    /// Where each round trip sits, by its door and request; each door's
    /// round trips; each wait by its kind; each file read by its path —
    /// every list ascending, so a lookup is a halving, never a walk of the
    /// segment (a candidate stuck asking again in a long segment asks up to
    /// the progress guard's count of times).
    by_request: BTreeMap<(String, Vec<u8>), Vec<usize>>,
    by_door: BTreeMap<String, Vec<usize>>,
    waits: BTreeMap<Option<String>, Vec<usize>>,
    by_path: BTreeMap<String, Vec<usize>>,
    /// Each wait by its door and target ([`wait_key`]): where the same wait
    /// sits whatever its timeout.
    wait_keys: BTreeMap<(String, Vec<String>), Vec<usize>>,
    /// Each housekeeping round trip by its door and kind
    /// ([`housekeeping_kind`]).
    housekeeping: BTreeMap<(String, Vec<String>), Vec<usize>>,
    /// The round trips and file reads a candidate may ask (every one but an
    /// output's): where the tape is spent ([`ActsCursor::spent`]).
    askable: Vec<usize>,
}

/// The first of the ascending `list` in `lo..hi`.
fn first_in(list: Option<&Vec<usize>>, lo: usize, hi: usize) -> Option<usize> {
    let list = list?;
    let i = list.partition_point(|&p| p < lo);
    list.get(i).copied().filter(|&p| p < hi)
}

/// The last of the ascending `list` in `lo..hi`.
fn last_in(list: Option<&Vec<usize>>, lo: usize, hi: usize) -> Option<usize> {
    let list = list?;
    let i = list.partition_point(|&p| p < hi);
    i.checked_sub(1)
        .and_then(|i| list.get(i).copied())
        .filter(|&p| p >= lo)
}

impl ActsCursor {
    /// A cursor over `records`, each with its stamp (`stamps[i]` is `records[i]`'s).
    pub(crate) fn new(records: Vec<EffectRecord>, mut stamps: Vec<Option<Stamp>>) -> Self {
        stamps.resize(records.len(), None);
        let acts: Vec<usize> = records
            .iter()
            .enumerate()
            .filter(|(_, r)| is_act_record(r))
            .map(|(at, _)| at)
            .collect();
        let mut raised_at = BTreeMap::new();
        let mut by_request: BTreeMap<(String, Vec<u8>), Vec<usize>> = BTreeMap::new();
        let mut by_door: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        let mut waits: BTreeMap<Option<String>, Vec<usize>> = BTreeMap::new();
        let mut by_path: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        let mut housekeeping: BTreeMap<(String, Vec<String>), Vec<usize>> = BTreeMap::new();
        let mut wait_keys: BTreeMap<(String, Vec<String>), Vec<usize>> = BTreeMap::new();
        let mut askable = Vec::new();
        for (at, r) in records.iter().enumerate() {
            match r {
                EffectRecord::Raised { door, .. } => {
                    raised_at.entry(door.clone()).or_insert(at);
                }
                EffectRecord::Exchange { door, request, .. } => {
                    by_request
                        .entry((door.clone(), request.clone()))
                        .or_default()
                        .push(at);
                    by_door.entry(door.clone()).or_default().push(at);
                    let class = act_class(door, request);
                    match class {
                        ActClass::Wait if door == doors::CTL => {
                            waits.entry(wait_of(request).0).or_default().push(at);
                            wait_keys
                                .entry((door.clone(), wait_key(request)))
                                .or_default()
                                .push(at);
                        }
                        ActClass::Housekeeping => {
                            housekeeping
                                .entry((door.clone(), housekeeping_kind(request)))
                                .or_default()
                                .push(at);
                        }
                        _ => {}
                    }
                    if class != ActClass::Output {
                        askable.push(at);
                    }
                }
                EffectRecord::File { path, .. } | EffectRecord::FileErr { path, .. } => {
                    by_path.entry(path.clone()).or_default().push(at);
                    askable.push(at);
                }
                _ => {}
            }
        }
        let base_wall = records
            .iter()
            .zip(&stamps)
            .find_map(|(r, s)| match (r, s) {
                (EffectRecord::Clock(ns), _) => Some(*ns as i64),
                (_, Some(s)) => Some(s.wall_ms.saturating_mul(1_000_000)),
                _ => None,
            })
            .unwrap_or(0);
        let account = ActsAccount {
            acts: acts.len(),
            ..ActsAccount::default()
        };
        Self {
            records,
            stamps,
            acts,
            raised_at,
            pos: 0,
            seg_start: 0,
            strayed: false,
            base_mono: 0,
            base_wall,
            slept_mono: 0,
            slept_wall: 0,
            mono_floor: 0,
            latched: BTreeSet::new(),
            flag_reads: BTreeMap::new(),
            idle: 0,
            account,
            by_request,
            by_door,
            waits,
            by_path,
            wait_keys,
            housekeeping,
            askable,
        }
    }

    /// The tape's records.
    pub(crate) fn records(&self) -> &[EffectRecord] {
        &self.records
    }

    /// The next record not consumed or dropped (the effect's ordinal).
    pub(crate) fn position(&self) -> usize {
        self.pos
    }

    /// The segment's closing act: the first act at or past the cursor.
    pub(crate) fn closing(&self) -> Option<usize> {
        let i = self.acts.partition_point(|&a| a < self.pos);
        self.acts.get(i).copied()
    }

    /// The end of the loop's segment: its closing act, or the tape's end.
    fn seg_end(&self) -> usize {
        self.closing().unwrap_or(self.records.len())
    }

    /// The first record of the loop's segment.
    pub(crate) fn segment_start(&self) -> usize {
        self.seg_start
    }

    /// The acts the candidate has not reached: the first, and how many.
    pub(crate) fn acts_left(&self) -> (Option<usize>, usize) {
        let i = self.acts.partition_point(|&a| a < self.pos);
        (self.acts.get(i).copied(), self.acts.len() - i)
    }

    /// Where the virtual clocks stand: monotonic ns since the tape's origin, wall ns since the epoch.
    pub(crate) fn now(&self) -> (u64, i64) {
        (
            self.mono_floor
                .max(self.base_mono.saturating_add(self.slept_mono)),
            self.base_wall
                .saturating_add(i64::try_from(self.slept_wall).unwrap_or(i64::MAX)),
        )
    }

    /// Take record `at` (at or past the cursor), dropping those before it.
    fn consume(&mut self, at: usize) {
        if at > self.pos {
            self.account.dropped += at - self.pos;
            self.strayed = true;
        }
        self.pos = at + 1;
        self.account.consumed += 1;
        self.idle = 0;
        self.flag_reads.clear();
        if let Some(Some(stamp)) = self.stamps.get(at) {
            self.base_mono = stamp.mono_ns;
            self.base_wall = stamp.wall_ms.saturating_mul(1_000_000);
            self.slept_mono = 0;
            self.slept_wall = 0;
        }
        match self.records.get(at) {
            Some(EffectRecord::Mono(ns)) => {
                self.base_mono = *ns;
                self.slept_mono = 0;
            }
            Some(EffectRecord::Clock(ns)) => {
                self.base_wall = *ns as i64;
                self.slept_wall = 0;
            }
            Some(r) if is_act_record(r) => {
                self.seg_start = self.pos;
                self.strayed = false;
                self.account.taken += 1;
            }
            _ => {}
        }
    }

    /// A request served without consuming a record.
    fn unconsumed(&mut self) {
        self.idle += 1;
        self.strayed = true;
    }

    /// Time passes with no record: a sleep, or a wait served again.
    fn pass(&mut self, ns: u64) {
        self.slept_mono = self.slept_mono.saturating_add(ns);
        self.slept_wall = self.slept_wall.saturating_add(ns);
    }

    /// Whether record `at` is a round trip at `door` asking `request`.
    fn holds(&self, at: usize, door: &str, request: &[u8]) -> bool {
        matches!(&self.records[at], EffectRecord::Exchange { door: d, request: r, .. }
            if d == door && r.as_slice() == request)
    }

    /// The reply of the round trip at `at`.
    fn reply(&self, at: usize) -> Vec<u8> {
        match &self.records[at] {
            EffectRecord::Exchange { reply, .. } => reply.clone(),
            _ => Vec::new(),
        }
    }

    /// Where the round trips at `door` asking `request` sit.
    fn asked(&self, door: &str, request: &[u8]) -> Option<&Vec<usize>> {
        self.by_request.get(&(door.to_string(), request.to_vec()))
    }

    /// The first of `list` in the segment ahead of the cursor.
    fn ahead(&self, list: Option<&Vec<usize>>) -> Option<usize> {
        first_in(list, self.pos, self.seg_end())
    }

    /// The latest of `list` in `from..` the cursor.
    fn behind(&self, list: Option<&Vec<usize>>, from: usize) -> Option<usize> {
        last_in(list, from, self.pos)
    }

    /// The segment's closing act, when the tape holds the wait `list` names
    /// only past it — what the recorded run waited for only once it had taken
    /// that act.
    fn skipped(&self, list: Option<&Vec<usize>>) -> Option<usize> {
        let closing = self.closing()?;
        first_in(list, closing + 1, self.records.len()).map(|_| closing)
    }

    /// Whether the tape is SPENT where the cursor stands: no round trip a candidate may ask (an output's aside) and no file read at or past it — what a tape with no end holds past its last record is not on it.
    pub(crate) fn spent(&self) -> bool {
        first_in(Some(&self.askable), self.pos, self.records.len()).is_none()
    }

    /// A DERIVED PROBE's answer from before the cursor: the latest round trip
    /// at `door` asking `request` that astream-effects' keyed rule admits at
    /// `seen = pos` — [`astream_effects::keyed_hit_within_cutoff`], the very
    /// predicate [`astream_effects::KeyedView::lookup`] admits a recorded
    /// answer by, so a change of astream's admission rule is acts mode's too
    /// (`a_keyed_answer_is_astreams_at_every_cursor` holds the two equal at
    /// every position of a generated tape).
    fn keyed(&self, door: &str, request: &[u8]) -> Option<usize> {
        let list = self.asked(door, request)?;
        let admitted =
            list.partition_point(|&at| astream_effects::keyed_hit_within_cutoff(at, self.pos));
        admitted.checked_sub(1).and_then(|i| list.get(i).copied())
    }

    /// Take the record ahead at `at`, its reply served.
    fn take(&mut self, at: usize) -> Served {
        self.consume(at);
        Served {
            reply: self.reply(at),
            at: Some(at),
        }
    }

    /// Serve the record behind at `at` again.
    fn again(&mut self, at: usize) -> Served {
        self.unconsumed();
        Served {
            reply: self.reply(at),
            at: Some(at),
        }
    }

    /// A ROUND TRIP at `door` asking `request` ([module docs](self)).
    pub(crate) fn exchange(&mut self, door: &str, request: &[u8]) -> Result<Served, Refusal> {
        if door == doors::SLEEP {
            return Ok(self.sleep(request));
        }
        let same = self.asked(door, request);
        match act_class(door, request) {
            ActClass::Act => match self.closing() {
                Some(c) if self.holds(c, door, request) => Ok(self.take(c)),
                closing => Err(Refusal::ActDiffers { closing }),
            },
            ActClass::Read => {
                if let Some(at) = self.ahead(same) {
                    return Ok(self.take(at));
                }
                if let Some(at) = self.behind(same, self.seg_start) {
                    self.account.repeated += 1;
                    return Ok(self.again(at));
                }
                if door.starts_with(doors::PROBE)
                    && let Some(at) = self.keyed(door, request)
                {
                    self.account.keyed += 1;
                    return Ok(self.again(at));
                }
                Err(Refusal::Stale {
                    class: ActClass::Read,
                })
            }
            ActClass::Wait => {
                let (kind, timeout) = wait_of(request);
                if let Some(at) = self.ahead(same) {
                    return Ok(self.take(at));
                }
                if let Some(at) = self.behind(same, self.seg_start) {
                    self.account.repeated += 1;
                    self.pass(timeout.unwrap_or(0).saturating_mul(1_000_000));
                    return Ok(self.again(at));
                }
                let kin = (door == doors::CTL)
                    .then(|| self.waits.get(&kind))
                    .flatten();
                if let Some(at) = self.ahead(kin) {
                    self.account.tainted += 1;
                    self.strayed = true;
                    return Ok(self.take(at));
                }
                let target = (door.to_string(), wait_key(request));
                Err(match self.skipped(self.wait_keys.get(&target)) {
                    Some(closing) => Refusal::Skipped {
                        closing,
                        class: ActClass::Wait,
                    },
                    None => Refusal::Stale {
                        class: ActClass::Wait,
                    },
                })
            }
            ActClass::Housekeeping => {
                if let Some(at) = self.ahead(same) {
                    return Ok(self.take(at));
                }
                if let Some(at) = self.behind(same, 0) {
                    if at < self.seg_start {
                        self.account.housekept += 1;
                    } else {
                        self.account.repeated += 1;
                    }
                    return Ok(self.again(at));
                }
                self.account.housekept += 1;
                let kind = (door.to_string(), housekeeping_kind(request));
                if let Some(at) = self.behind(self.housekeeping.get(&kind), 0) {
                    return Ok(self.again(at));
                }
                self.unconsumed();
                Ok(Served {
                    reply: lane_success(),
                    at: None,
                })
            }
            ActClass::Output => {
                let at_door = |at: usize| matches!(&self.records[at], EffectRecord::Exchange { door: d, .. } if d == door);
                if self.pos < self.seg_end() && at_door(self.pos) {
                    let at = self.pos;
                    return Ok(self.take(at));
                }
                let served = match self.behind(self.by_door.get(door), 0) {
                    Some(at) => Served {
                        reply: self.reply(at),
                        at: Some(at),
                    },
                    None => Served {
                        reply: Vec::new(),
                        at: None,
                    },
                };
                self.account.unmatched_outputs += 1;
                self.unconsumed();
                Ok(served)
            }
        }
    }

    /// A SLEEP whose request is its length in nanoseconds: the identical
    /// sleep under the cursor, else virtual time.
    fn sleep(&mut self, request: &[u8]) -> Served {
        if self.pos < self.records.len() && self.holds(self.pos, doors::SLEEP, request) {
            let at = self.pos;
            return self.take(at);
        }
        let ns = std::str::from_utf8(request)
            .ok()
            .and_then(|t| t.parse::<u64>().ok())
            .unwrap_or(0);
        self.account.slept += 1;
        self.unconsumed();
        self.pass(ns);
        Served {
            reply: Vec::new(),
            at: None,
        }
    }

    /// An OUTPUT at `door`, never compared: the record under the cursor taken when it is one of that door.
    pub(crate) fn emit(&mut self, door: &str) -> Option<usize> {
        let at = self.pos;
        if at < self.seg_end()
            && matches!(&self.records[at], EffectRecord::Emit { door: d, .. } if d == door)
        {
            self.consume(at);
            return Some(at);
        }
        self.account.unmatched_outputs += 1;
        self.unconsumed();
        None
    }

    /// A MONOTONIC read: the record under the cursor, else virtual time.
    pub(crate) fn mono(&mut self) -> u64 {
        if let Some(EffectRecord::Mono(ns)) = self.records.get(self.pos) {
            let ns = *ns;
            let at = self.pos;
            self.consume(at);
            self.mono_floor = self.mono_floor.max(ns);
            return self.mono_floor;
        }
        self.account.virtual_clock += 1;
        self.unconsumed();
        let (now, _) = self.now();
        self.mono_floor = now;
        now
    }

    /// A WALL read (ns since the epoch, two's complement): the record under the cursor, else virtual time.
    pub(crate) fn clock(&mut self) -> u64 {
        if let Some(EffectRecord::Clock(ns)) = self.records.get(self.pos) {
            let ns = *ns;
            let at = self.pos;
            self.consume(at);
            return ns;
        }
        self.account.virtual_clock += 1;
        self.unconsumed();
        self.now().1 as u64
    }

    /// A FILE read at `path` (as the run's roots write it): the first record of the segment that read it, else the latest the segment already did, else STALE.
    pub(crate) fn file(&mut self, path: &str) -> Result<io::Result<Vec<u8>>, Refusal> {
        let read = self.by_path.get(path);
        let outcome = |r: &EffectRecord| match r {
            EffectRecord::File { bytes, .. } => Ok(bytes.clone()),
            EffectRecord::FileErr { kind, .. } => Err(io::Error::from(*kind)),
            _ => Err(io::Error::other("no read")),
        };
        if let Some(at) = self.ahead(read) {
            self.consume(at);
            return Ok(outcome(&self.records[at]));
        }
        if let Some(at) = self.behind(read, self.seg_start) {
            self.account.repeated += 1;
            self.unconsumed();
            return Ok(outcome(&self.records[at]));
        }
        Err(Refusal::Stale {
            class: ActClass::Read,
        })
    }

    /// A FLAG read at `door` ([module docs](self)).
    pub(crate) fn raised(&mut self, door: &str) -> bool {
        if self.latched.contains(door) {
            return true;
        }
        let Some(&at) = self.raised_at.get(door) else {
            self.idle += 1;
            return false;
        };
        let up = if at < self.pos {
            true
        } else if at == self.pos {
            let false_reads = match &self.records[at] {
                EffectRecord::Raised { false_reads, .. } => *false_reads,
                _ => 0,
            };
            let read = self.flag_reads.entry(door.to_string()).or_insert(0);
            if *read >= false_reads {
                self.consume(at);
                true
            } else {
                *read += 1;
                false
            }
        } else if self.strayed
            && at < self.seg_end()
            && self.stamps[at].is_some_and(|s| self.now().0 >= s.mono_ns)
        {
            self.consume(at);
            true
        } else {
            false
        };
        if up {
            self.latched.insert(door.to_string());
        } else {
            self.idle += 1;
        }
        up
    }
}

#[cfg(test)]
#[path = "acts_tests.rs"]
mod tests;
