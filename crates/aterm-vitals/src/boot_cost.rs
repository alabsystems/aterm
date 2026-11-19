// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded boot-cost observations at real continuous-time cuts.
//!
//! Target times select observations, never fabricate them. Raw counter channels remain
//! independent published accounting; no cut establishes an execution-window bound.

use crate::explain::HistoricalReported;
use crate::published_cpu::CpuAccounting;

#[path = "boot_cost_codec.rs"]
mod codec;
pub use codec::{decode, encode};

/// The two uptime targets, in continuous nanoseconds since boot, including sleep.
pub const TARGETS_NS: [u64; 2] = [180_000_000_000, 900_000_000_000];
/// Maximum retained boots, oldest first.
pub const MAX_BOOTS: usize = 8;
/// Maximum encoded document bytes.
pub const MAX_BYTES: usize = 128 * 1024;
/// Maximum delivered-UI wake variants per observation.
pub const MAX_VARIANTS: usize = 256;
/// Maximum stable boot-identity bytes.
pub const MAX_BOOT_ID_BYTES: usize = 128;
/// Maximum bytes in one delivered-UI wake variant's name.
pub const MAX_WAKE_KIND_BYTES: usize = 96;

/// A rejected observation or document; rejection never changes retained cuts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The document has another magic or version.
    Format,
    /// The byte cap was exceeded.
    TooLarge,
    /// A count or string exceeded its bound.
    Bounds,
    /// A declared field was truncated.
    Truncated,
    /// A string was not UTF-8.
    Utf8,
    /// Bytes remained after the document.
    Trailing,
    /// A field combination was invalid.
    Invalid(&'static str),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Format => f.write_str("unsupported boot-cost format"),
            Self::TooLarge => f.write_str("boot-cost byte cap exceeded"),
            Self::Bounds => f.write_str("boot-cost field bound exceeded"),
            Self::Truncated => f.write_str("truncated boot-cost document"),
            Self::Utf8 => f.write_str("invalid boot-cost UTF-8"),
            Self::Trailing => f.write_str("trailing boot-cost bytes"),
            Self::Invalid(why) => write!(f, "invalid boot-cost {why}"),
        }
    }
}
impl std::error::Error for Error {}

/// Qualifications of observed accounting, separate from execution-window assurance.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Coverage(pub u16);
impl Coverage {
    /// Some source was incomplete.
    pub const PARTIAL: Self = Self(1);
    /// The collector first observed this boot after continuous time zero.
    pub const LATE_START: Self = Self(1 << 1);
    /// A target is represented by an observation after that target.
    pub const LATE_CUT: Self = Self(1 << 2);
    /// Observation continuity was interrupted.
    pub const COLLECTION_GAP: Self = Self(1 << 3);
    /// A counter epoch changed or a comparable counter decreased.
    pub const COUNTER_RESET: Self = Self(1 << 4);
    /// A process or coalition identity changed.
    pub const SOURCE_CHANGED: Self = Self(1 << 5);
    /// Delivered-UI wake epochs changed or comparable counters decreased.
    pub const WAKE_RESET: Self = Self(1 << 6);
    /// Two retained documents disagreed about an occupied target.
    pub const CONFLICT: Self = Self(1 << 7);
    /// Process generation evidence was missing.
    pub const IDENTITY_MISSING: Self = Self(1 << 8);
    /// One or more raw accounting counters were unavailable.
    pub const COUNTERS_MISSING: Self = Self(1 << 9);
    /// Delivered-UI wake observations were unavailable.
    pub const WAKE_MISSING: Self = Self(1 << 10);
    /// Restored data could not establish continuity to this collector's first reading.
    pub const RESTORED_GAP: Self = Self(1 << 11);
    const KNOWN: u16 = (1 << 12) - 1;

    /// Retain both sets of qualifications.
    #[must_use]
    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
    /// Whether every qualification of `other` is present.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
    /// Whether any collection qualification is present; this is no execution assurance.
    #[must_use]
    pub const fn is_partial(self) -> bool {
        self.0 != 0
    }
    /// Stable words for the qualifications that are present.
    #[must_use]
    pub fn words(self) -> Vec<&'static str> {
        [
            (Self::PARTIAL, "partial"),
            (Self::LATE_START, "late_start"),
            (Self::LATE_CUT, "late_cut"),
            (Self::COLLECTION_GAP, "collection_gap"),
            (Self::COUNTER_RESET, "counter_reset"),
            (Self::SOURCE_CHANGED, "source_changed"),
            (Self::WAKE_RESET, "wake_reset"),
            (Self::CONFLICT, "conflict"),
            (Self::IDENTITY_MISSING, "identity_unavailable"),
            (Self::COUNTERS_MISSING, "counter_unavailable"),
            (Self::WAKE_MISSING, "wake_unavailable"),
            (Self::RESTORED_GAP, "restored_gap"),
        ]
        .into_iter()
        .filter_map(|(flag, word)| self.contains(flag).then_some(word))
        .collect()
    }
}

/// One observed process generation; native start units remain the producer's units.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProcessKey {
    /// Process identifier.
    pub pid: u32,
    /// Kernel start identity, not an elapsed duration.
    pub start: u64,
    /// Executable-generation version, where observed.
    pub version: Option<i32>,
}
/// Independent raw lifetime counters of one source.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RawCounters {
    /// Published own CPU nanoseconds.
    pub own_ns: Option<u64>,
    /// Published inherited/reaped CPU nanoseconds; may repeat descendant accounting.
    pub inherited_ns: Option<u64>,
}
/// One lifetime delivered-UI wake counter, never an all-system wake count.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WakeCount {
    /// Variant name as the frontend named it.
    pub kind: String,
    /// Lifetime delivered count in the named epoch.
    pub count: u64,
}
/// Delivered-UI wake observations in one process-local epoch.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WakeCounts {
    /// Process-local epoch, keyed by [`Observation::process`]; a metrics reset does not change it.
    /// The same number in another process does not establish continuity.
    pub epoch: u64,
    /// Independently observed per-variant counters.
    pub variants: Vec<WakeCount>,
}
/// One actual full observation, retained unchanged once a target references it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Observation {
    /// Actual capture cut, continuous nanoseconds since boot, including sleep.
    pub at_ns: u64,
    /// Continuous clock before the source reads, where observed.
    pub before_ns: Option<u64>,
    /// Continuous clock after the source reads, no later than `at_ns`.
    pub after_ns: Option<u64>,
    /// Producer identity of the process CPU and delivered-UI wake counters.
    pub process: Option<ProcessKey>,
    /// Process-local counter epoch, keyed by `process` and independent of the read bracket.
    /// The same number in another process does not establish continuity.
    pub counter_epoch: u64,
    /// Current process raw counters.
    pub process_cpu: RawCounters,
    /// Identity of the independently observed app coalition.
    pub coalition_id: Option<u64>,
    /// Raw coalition counters; never added to the process or inherited channel.
    pub coalition_cpu: RawCounters,
    /// Accepted historical published growth by original assignment, including closed tabs.
    pub historical_reported: HistoricalReported,
    /// Lifetime delivered-UI variant observations, where available.
    pub wakes: Option<WakeCounts>,
    /// Producer and collector qualifications at this cut.
    pub coverage: Coverage,
}
impl Observation {
    /// The only accounting basis this producer supplies; no execution-window assurance.
    #[must_use]
    pub const fn accounting(&self) -> CpuAccounting {
        CpuAccounting::PUBLISHED_COUNTERS
    }
}
/// The observations of one stable boot identity. Targets index actual observations.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Boot {
    /// Stable native boot identity.
    pub boot_id: String,
    /// The collector's first actual continuous-time observation of this boot.
    pub first_seen_ns: u64,
    /// At most two distinct actual observations, oldest first.
    pub observations: Vec<Observation>,
    /// Observation indexes for the 180 s and 900 s targets; two may name the same cut.
    pub targets: [Option<u8>; 2],
    /// Qualifications accumulated across reads and merges; never removed by a later read.
    pub coverage: Coverage,
}
impl Boot {
    /// The actual observation a target references.
    #[must_use]
    pub fn observation(&self, target: usize) -> Option<&Observation> {
        self.targets
            .get(target)
            .copied()
            .flatten()
            .and_then(|i| self.observations.get(usize::from(i)))
    }
    /// Qualifications for a target, including document-merge conflicts.
    #[must_use]
    pub fn coverage_for(&self, target: usize) -> Coverage {
        self.observation(target)
            .map_or(self.coverage.with(Coverage::PARTIAL), |o| {
                self.coverage.with(o.coverage)
            })
    }
    fn complete(&self) -> bool {
        self.targets.iter().all(Option::is_some)
    }
}
/// The bounded stored document. Persistence status belongs to its native owner, not this data.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Book {
    /// Retained boot records, oldest observed boot first.
    pub boots: Vec<Boot>,
}
impl Book {
    /// The record of a stable boot identity, where retained.
    #[must_use]
    pub fn boot(&self, boot_id: &str) -> Option<&Boot> {
        self.boots.iter().find(|b| b.boot_id == boot_id)
    }
    /// The next unresolved target; `None` once both targets have actual observations.
    #[must_use]
    pub fn next_target_ns(&self, boot_id: &str) -> Option<u64> {
        if !valid_label(boot_id, MAX_BOOT_ID_BYTES) {
            return None;
        }
        match self.boot(boot_id) {
            Some(boot) => boot
                .targets
                .iter()
                .position(Option::is_none)
                .map(|i| TARGETS_NS[i]),
            None => Some(TARGETS_NS[0]),
        }
    }
    /// Merge an incoming valid document. Occupied targets in `self` remain authoritative;
    /// disagreement marks a conflict instead of replacing or summing observations.
    ///
    /// # Errors
    /// Rejects either invalid or oversized document without changing `self`.
    pub fn merge(&mut self, incoming: &Book) -> Result<bool, Error> {
        let _ = encode(self)?;
        let _ = encode(incoming)?;
        let mut out = self.clone();
        for other in &incoming.boots {
            match out.boots.iter().position(|b| b.boot_id == other.boot_id) {
                None => out.boots.push(other.clone()),
                Some(index) => {
                    let mut boot = out.boots.remove(index);
                    merge_boot(&mut boot, other);
                    out.boots.push(boot);
                }
            }
        }
        out.trim()?;
        let changed = out != *self;
        *self = out;
        Ok(changed)
    }
    fn trim(&mut self) -> Result<(), Error> {
        while self.boots.len() > MAX_BOOTS {
            self.boots.remove(0);
        }
        loop {
            match encode(self) {
                Ok(_) => return Ok(()),
                Err(Error::TooLarge) if self.boots.len() > 1 => {
                    self.boots.remove(0);
                }
                Err(error) => return Err(error),
            }
        }
    }
}

/// Pure runtime collector. Its last-reading cursor is deliberately absent from stored `Book`.
#[derive(Clone, Debug)]
pub struct Collector {
    book: Book,
    last: Option<(String, Observation)>,
    restored: bool,
}
impl Collector {
    /// Begin with a restored bounded document; no observation is invented from its targets.
    /// Observations with a boot identity and restoration reject invalid input.
    #[must_use]
    pub fn new(book: Book) -> Self {
        let restored = !book.boots.is_empty();
        Self {
            book,
            last: None,
            restored,
        }
    }
    /// The document to offer the native atomic store. A failed write does not change it.
    #[must_use]
    pub fn book(&self) -> &Book {
        &self.book
    }
    /// Incorporate the native store's authoritative persisted view, retaining pending new cuts.
    ///
    /// # Errors
    /// Rejects invalid or oversized input without changing the collector.
    pub fn restore(&mut self, saved: &Book) -> Result<bool, Error> {
        let mut merged = saved.clone();
        merged.merge(&self.book)?;
        let changed = merged != self.book;
        if changed {
            self.book = merged;
            self.last = None;
            self.restored = true;
        }
        Ok(changed)
    }
    /// Observe one actual cut. An unavailable boot identity records nothing. Resolved targets
    /// are immutable, including after restoration; repeated reads add no duplicate cuts.
    ///
    /// # Errors
    /// Rejects invalid fields or restored documents, and continuous-clock regressions while
    /// targets remain unresolved, without changing the collector. Both cuts must fit the byte cap.
    pub fn observe(
        &mut self,
        boot_id: Option<&str>,
        mut observation: Observation,
    ) -> Result<bool, Error> {
        let Some(id) = boot_id else {
            return Ok(false);
        };
        if !valid_label(id, MAX_BOOT_ID_BYTES) {
            return Err(Error::Invalid("boot identity"));
        }
        let _ = encode(&self.book)?;
        validate_observation(&observation)?;
        if self.book.boot(id).is_some_and(Boot::complete) {
            return Ok(false);
        }
        let previous = self
            .last
            .as_ref()
            .filter(|(boot, _)| boot == id)
            .map(|(_, o)| o)
            .or_else(|| self.book.boot(id).and_then(|b| b.observations.last()));
        if previous.is_some_and(|p| observation.at_ns < p.at_ns) {
            return Err(Error::Invalid("regressed continuous clock"));
        }
        observation.coverage = observed_coverage(&observation, previous);
        if self.restored && self.book.boot(id).is_some() {
            observation.coverage = observation
                .coverage
                .with(Coverage::PARTIAL)
                .with(Coverage::COLLECTION_GAP)
                .with(Coverage::RESTORED_GAP);
        }
        let mut next = self.book.clone();
        let index = match next.boots.iter().position(|b| b.boot_id == id) {
            Some(i) => i,
            None => {
                next.boots.push(Boot {
                    boot_id: id.to_owned(),
                    first_seen_ns: observation.at_ns,
                    ..Boot::default()
                });
                next.boots.len() - 1
            }
        };
        let mut boot = next.boots.remove(index);
        if boot.first_seen_ns > 0 {
            observation.coverage = observation
                .coverage
                .with(Coverage::PARTIAL)
                .with(Coverage::LATE_START);
        }
        let due: Vec<usize> = TARGETS_NS
            .iter()
            .enumerate()
            .filter_map(|(i, target)| {
                (boot.targets[i].is_none() && observation.at_ns >= *target).then_some(i)
            })
            .collect();
        if due.iter().any(|&i| observation.at_ns > TARGETS_NS[i]) {
            observation.coverage = observation
                .coverage
                .with(Coverage::PARTIAL)
                .with(Coverage::LATE_CUT);
        }
        boot.coverage = boot.coverage.with(observation.coverage);
        if !due.is_empty() {
            let slot = u8::try_from(boot.observations.len()).map_err(|_| Error::Bounds)?;
            if slot >= 2 {
                return Err(Error::Bounds);
            }
            boot.observations.push(observation.clone());
            for target in due {
                boot.targets[target] = Some(slot);
            }
        }
        next.boots.push(boot);
        next.trim()?;
        let changed = self.book != next;
        self.book = next;
        self.last = Some((id.to_owned(), observation));
        self.restored = false;
        Ok(changed)
    }
}

fn observed_coverage(now: &Observation, before: Option<&Observation>) -> Coverage {
    let mut c = now.coverage;
    if c.0 != 0 {
        c = c.with(Coverage::PARTIAL);
    }
    if now.before_ns.is_none() || now.after_ns.is_none() {
        c = c.with(Coverage::PARTIAL);
    }
    if now
        .process
        .is_none_or(|p| p.pid == 0 || p.version.is_none())
    {
        c = c.with(Coverage::PARTIAL).with(Coverage::IDENTITY_MISSING);
    }
    if now.process_cpu.own_ns.is_none()
        || now.process_cpu.inherited_ns.is_none()
        || now.coalition_id.is_none()
        || now.coalition_cpu.own_ns.is_none()
        || now.coalition_cpu.inherited_ns.is_none()
    {
        c = c.with(Coverage::PARTIAL).with(Coverage::COUNTERS_MISSING);
    }
    if now.historical_reported.incomplete {
        c = c.with(Coverage::PARTIAL);
    }
    if now.wakes.is_none() {
        c = c.with(Coverage::PARTIAL).with(Coverage::WAKE_MISSING);
    }
    if now
        .wakes
        .as_ref()
        .is_some_and(|w| w.variants.iter().any(|v| v.count == u64::MAX))
    {
        c = c.with(Coverage::PARTIAL);
    }
    if let Some(old) = before {
        if now.process != old.process || now.coalition_id != old.coalition_id {
            c = c.with(Coverage::PARTIAL).with(Coverage::SOURCE_CHANGED);
        }
        if now.counter_epoch != old.counter_epoch
            || (now.process == old.process && counters_decreased(now.process_cpu, old.process_cpu))
            || (now.coalition_id == old.coalition_id
                && counters_decreased(now.coalition_cpu, old.coalition_cpu))
            || now.historical_reported.own_ns < old.historical_reported.own_ns
            || now.historical_reported.tabs_ns < old.historical_reported.tabs_ns
        {
            c = c.with(Coverage::PARTIAL).with(Coverage::COUNTER_RESET);
        }
        if let (Some(a), Some(b)) = (&now.wakes, &old.wakes)
            && (now.process != old.process
                || a.epoch != b.epoch
                || b.variants.iter().any(|old| {
                    a.variants
                        .iter()
                        .find(|n| n.kind == old.kind)
                        .is_none_or(|n| n.count < old.count)
                }))
        {
            c = c.with(Coverage::PARTIAL).with(Coverage::WAKE_RESET);
        }
    }
    c
}
fn counters_decreased(now: RawCounters, old: RawCounters) -> bool {
    [
        (now.own_ns, old.own_ns),
        (now.inherited_ns, old.inherited_ns),
    ]
    .into_iter()
    .any(|(n, o)| matches!((n, o), (Some(n), Some(o)) if n < o))
}
fn merge_boot(boot: &mut Boot, other: &Boot) {
    boot.first_seen_ns = boot.first_seen_ns.min(other.first_seen_ns);
    boot.coverage = boot.coverage.with(other.coverage);
    for target in 0..2 {
        let Some(incoming) = other.observation(target) else {
            continue;
        };
        if let Some(existing) = boot.observation(target) {
            if existing != incoming {
                boot.coverage = boot
                    .coverage
                    .with(Coverage::PARTIAL)
                    .with(Coverage::CONFLICT);
            }
            continue;
        }
        let found = boot.observations.iter().position(|o| o == incoming);
        let index = match found {
            Some(i) => i,
            None if boot.observations.len() < 2
                && !boot.observations.iter().any(|o| o.at_ns == incoming.at_ns) =>
            {
                if let Some(old) = boot.observations.last() {
                    boot.coverage = boot.coverage.with(observed_coverage(incoming, Some(old)));
                }
                boot.observations.push(incoming.clone());
                boot.observations.len() - 1
            }
            None => {
                boot.coverage = boot
                    .coverage
                    .with(Coverage::PARTIAL)
                    .with(Coverage::CONFLICT);
                continue;
            }
        };
        boot.targets[target] = Some(index as u8);
    }
    if boot.observations.len() == 2 && boot.observations[0].at_ns > boot.observations[1].at_ns {
        boot.observations.swap(0, 1);
        for slot in boot.targets.iter_mut().flatten() {
            *slot = 1 - *slot;
        }
    }
}
fn valid_label(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && value.bytes().all(|b| b.is_ascii_graphic())
}
fn validate_observation(o: &Observation) -> Result<(), Error> {
    if o.coverage.0 & !Coverage::KNOWN != 0 {
        return Err(Error::Invalid("coverage flags"));
    }
    match (o.before_ns, o.after_ns) {
        (None, None) => {}
        (Some(a), Some(b)) if a <= b && b <= o.at_ns => {}
        _ => return Err(Error::Invalid("observation bracket")),
    }
    if o.process.is_some_and(|p| p.pid == 0) {
        return Err(Error::Invalid("process identity"));
    }
    if let Some(w) = &o.wakes {
        if w.variants.len() > MAX_VARIANTS {
            return Err(Error::Bounds);
        }
        for (i, v) in w.variants.iter().enumerate() {
            if !valid_label(&v.kind, MAX_WAKE_KIND_BYTES) {
                return Err(Error::Invalid("wake kind"));
            }
            if w.variants[..i].iter().any(|before| before.kind == v.kind) {
                return Err(Error::Invalid("duplicate wake kind"));
            }
        }
    }
    Ok(())
}
fn validate_book(book: &Book) -> Result<(), Error> {
    if book.boots.len() > MAX_BOOTS {
        return Err(Error::Bounds);
    }
    for (i, b) in book.boots.iter().enumerate() {
        if !valid_label(&b.boot_id, MAX_BOOT_ID_BYTES) {
            return Err(Error::Invalid("boot identity"));
        }
        if book.boots[..i].iter().any(|old| old.boot_id == b.boot_id) {
            return Err(Error::Invalid("duplicate boot identity"));
        }
        if b.coverage.0 & !Coverage::KNOWN != 0 || b.observations.len() > 2 {
            return Err(Error::Bounds);
        }
        for (j, o) in b.observations.iter().enumerate() {
            validate_observation(o)?;
            if !o.coverage.contains(observed_coverage(o, None)) {
                return Err(Error::Invalid("unqualified missing source"));
            }
            if o.at_ns < b.first_seen_ns
                || !b.targets.contains(&Some(j as u8))
                || b.coverage.0 & o.coverage.0 != o.coverage.0
            {
                return Err(Error::Invalid("unreferenced or unqualified observation"));
            }
            if j > 0 && b.observations[j - 1].at_ns >= o.at_ns {
                return Err(Error::Invalid("observation order"));
            }
            if j > 0
                && !b
                    .coverage
                    .contains(observed_coverage(o, Some(&b.observations[j - 1])))
            {
                return Err(Error::Invalid("unqualified observation transition"));
            }
        }
        if b.coverage.0 != 0 && !b.coverage.contains(Coverage::PARTIAL) {
            return Err(Error::Invalid("unqualified coverage"));
        }
        if b.first_seen_ns > 0 && !b.coverage.contains(Coverage::LATE_START) {
            return Err(Error::Invalid("unqualified late start"));
        }
        if b.targets[0].is_none() && b.targets[1].is_some() {
            return Err(Error::Invalid("target prefix"));
        }
        if b.observations
            .first()
            .is_some_and(|o| o.at_ns >= TARGETS_NS[1])
            && b.targets != [Some(0), Some(0)]
        {
            return Err(Error::Invalid("shared late cut"));
        }
        for (target, slot) in b.targets.iter().enumerate() {
            if let Some(slot) = slot {
                let Some(o) = b.observations.get(usize::from(*slot)) else {
                    return Err(Error::Invalid("target index"));
                };
                if o.at_ns < TARGETS_NS[target] {
                    return Err(Error::Invalid("invented target cut"));
                }
                if o.at_ns > TARGETS_NS[target] && !b.coverage.contains(Coverage::LATE_CUT) {
                    return Err(Error::Invalid("unqualified late cut"));
                }
            } else if b.first_seen_ns >= TARGETS_NS[target] {
                return Err(Error::Invalid("missing first cut"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "boot_cost_tests.rs"]
mod tests;
