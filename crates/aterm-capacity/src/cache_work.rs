// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Validated, bounded observations of Targo build-script input capture costs.
//! These measurements establish neither build success, proof nor resource custody.

use serde::{Deserialize, Deserializer, de};
use std::{fmt, io, marker::PhantomData};

/// The only accepted compiler-work report schema.
pub const SCHEMA: &str = "targo-cache-work-report-v1";
/// Measured input scanning only; generated-output validation is outside this scope.
pub const SCOPE: &str = "build-script-inputs-v1";
/// Maximum serialized report accepted before JSON allocation.
pub const MAX_REPORT_BYTES: usize = 16 * 1024 * 1024;
/// Maximum registered unit rows, matching the producer's reader bound.
pub const MAX_UNITS: usize = 65_536;
/// Maximum complete capture attempts, matching the producer's reader bound.
pub const MAX_ATTEMPTS: u64 = 262_144;
const MAX_FEATURES: usize = 4096;
const MAX_TEXT_BYTES: usize = 32 * 1024;

/// Explicit availability; a legacy observation is never a measured zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Availability {
    /// The producer validated its complete capture-attempt denominator.
    Complete,
    /// The original analysis log predates input-work instrumentation.
    UnavailableLegacyLog,
}

/// A failed collection, distinct from a legacy report or an absent request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnavailableReason {
    /// No measurement was recorded for this work.
    NotRecorded,
    /// The original driver does not support this report contract.
    UnsupportedDriver,
    /// No matching analysis session was produced.
    MissingSession,
    /// More than one candidate prevented exact session selection.
    AmbiguousSession,
    /// The actual report command failed.
    QueryFailed,
    /// The report bytes failed bounded parsing or conservation checks.
    InvalidReport,
    /// Valid observations could not be preserved in their owned result storage.
    PersistenceFailed,
}

/// An actual collection result. Neither arm grants compiler or resource authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Collection {
    /// Exact-run, validated compiler observations.
    Report(Report),
    /// Collection failed without inventing zero costs or a run identity.
    Unavailable(UnavailableReason),
}

/// Eight independent work counters. Partial failed captures can contain work.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Work {
    /// Entries charged by the scanner.
    pub entries: u64,
    /// Input lengths encountered, independently of bytes actually hashed.
    pub input_bytes: u64,
    /// Actual bytes hashed.
    pub bytes_hashed: u64,
    /// Actual path metadata reads.
    pub metadata_reads: u64,
    /// Resolver component steps.
    pub resolution_steps: u64,
    /// Symlink target reads.
    pub link_reads: u64,
    /// Logical observations charged to capture bounds.
    pub observations: u64,
    /// Final original-observation revalidations.
    pub revalidations: u64,
}

impl Work {
    fn add(&mut self, other: Self) -> io::Result<()> {
        macro_rules! add {
            ($($field:ident),+ $(,)?) => {$(
                self.$field = self.$field.checked_add(other.$field).ok_or_else(bad)?;
            )+};
        }
        add!(
            entries,
            input_bytes,
            bytes_hashed,
            metadata_reads,
            resolution_steps,
            link_reads,
            observations,
            revalidations
        );
        Ok(())
    }
}

/// Original graph and capture denominators, never CPU utilization.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Denominators {
    /// All rows in the reported unit graph.
    pub registered_units: u64,
    /// All run-custom-build rows, including unobserved ones.
    pub run_script_units: u64,
    /// Units with at least one input capture attempt.
    pub observed_units: u64,
    /// Registered rows without any capture attempt.
    pub units_without_attempts: u64,
    /// Original started capture count.
    pub attempts_started: u64,
    /// Original finished capture count.
    pub attempts_finished: u64,
    /// Successfully captured attempts.
    pub captured_attempts: u64,
    /// Attempts which could not establish a reusable snapshot.
    pub uncacheable_attempts: u64,
}

/// Small wire projection. Consumers must validate decoded DTOs before publication.
/// Full unit graphs do not belong in the bounded host snapshot frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    /// Exact original analysis run, never a ticket or control capability.
    pub run_id: String,
    /// Explicit measured or legacy availability.
    pub availability: Availability,
    /// Original denominator observations.
    pub denominators: Denominators,
    /// Complete measured costs, or unavailable legacy costs.
    pub totals: Option<Work>,
}

impl Summary {
    /// The fixed measured scope; no decoded string can broaden it.
    #[must_use]
    pub const fn scope(&self) -> &'static str {
        SCOPE
    }

    /// Check the invariants visible in a bounded summary. Full row conservation
    /// is established only by [`parse`].
    /// # Errors
    /// Returns `InvalidData` for malformed identity, bounds, counts or nullability.
    pub fn validate(&self) -> io::Result<()> {
        let d = self.denominators;
        if !valid_run(&self.run_id)
            || d.registered_units > MAX_UNITS as u64
            || d.run_script_units > d.registered_units
            || d.observed_units > d.run_script_units
            || d.observed_units.checked_add(d.units_without_attempts) != Some(d.registered_units)
            || d.attempts_started > MAX_ATTEMPTS
            || d.attempts_finished > MAX_ATTEMPTS
            || d.attempts_started != d.attempts_finished
            || d.captured_attempts.checked_add(d.uncacheable_attempts) != Some(d.attempts_finished)
            || d.observed_units > d.attempts_finished
            || (d.observed_units == 0) != (d.attempts_finished == 0)
            || (d.attempts_finished == 0 && self.totals.is_some_and(|work| work != Work::default()))
        {
            return Err(bad());
        }
        match self.availability {
            Availability::Complete if self.totals.is_some() => Ok(()),
            Availability::UnavailableLegacyLog
                if self.totals.is_none() && d.observed_units == 0 && d.attempts_finished == 0 =>
            {
                Ok(())
            }
            _ => Err(bad()),
        }
    }
}

/// One capture phase, in the producer's stable phase order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    /// Metadata guard around actual build-script execution.
    ExecutionWindow,
    /// Content validation of prior declared inputs.
    Freshness,
    /// Content publication after actual build-script execution.
    Publication,
}

/// Compiler action, without importing compiler implementation types.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    /// Compile a test.
    Test,
    /// Compile code.
    Build,
    /// Check code.
    Check,
    /// Generate documentation.
    Doc,
    /// Run documentation tests.
    Doctest,
    /// Scrape documentation examples.
    Docscrape,
    /// Execute a compiled build script.
    RunCustomBuild,
}

/// Validated phase totals within one original unit.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhaseWork {
    phase: Phase,
    attempts: u64,
    captured_attempts: u64,
    uncacheable_attempts: u64,
    #[serde(deserialize_with = "object")]
    work: Work,
}

impl PhaseWork {
    /// Original phase.
    #[must_use]
    pub const fn phase(&self) -> Phase {
        self.phase
    }
    /// Complete phase attempt count.
    #[must_use]
    pub const fn attempts(&self) -> u64 {
        self.attempts
    }
    /// Successful captures.
    #[must_use]
    pub const fn captured_attempts(&self) -> u64 {
        self.captured_attempts
    }
    /// Captures refused after any reported partial work.
    #[must_use]
    pub const fn uncacheable_attempts(&self) -> u64 {
        self.uncacheable_attempts
    }
    /// Checked sum of the phase's measured work.
    #[must_use]
    pub const fn work(&self) -> Work {
        self.work
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct Target {
    name: String,
    kind: String,
}

/// One immutable original unit identity and its validated costs.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Unit {
    index: u64,
    package_id: String,
    #[serde(deserialize_with = "object")]
    target: Target,
    mode: Mode,
    platform: String,
    #[serde(deserialize_with = "features")]
    features: Vec<String>,
    requested: bool,
    attempts: u64,
    captured_attempts: u64,
    uncacheable_attempts: u64,
    #[serde(deserialize_with = "phases")]
    phases: Vec<PhaseWork>,
    #[serde(deserialize_with = "nullable_object")]
    work: Option<Work>,
}

impl Unit {
    /// Original graph index; indices need not be contiguous.
    #[must_use]
    pub const fn index(&self) -> u64 {
        self.index
    }
    /// Original package identity.
    #[must_use]
    pub fn package_id(&self) -> &str {
        &self.package_id
    }
    /// Original target name.
    #[must_use]
    pub fn target_name(&self) -> &str {
        &self.target.name
    }
    /// Original target kind.
    #[must_use]
    pub fn target_kind(&self) -> &str {
        &self.target.kind
    }
    /// Original compiler action.
    #[must_use]
    pub const fn mode(&self) -> Mode {
        self.mode
    }
    /// Original platform label.
    #[must_use]
    pub fn platform(&self) -> &str {
        &self.platform
    }
    /// Original enabled features.
    #[must_use]
    pub fn features(&self) -> &[String] {
        &self.features
    }
    /// Whether this was an explicitly requested unit.
    #[must_use]
    pub const fn requested(&self) -> bool {
        self.requested
    }
    /// Complete capture attempt count for this unit.
    #[must_use]
    pub const fn attempts(&self) -> u64 {
        self.attempts
    }
    /// Successful captures for this unit.
    #[must_use]
    pub const fn captured_attempts(&self) -> u64 {
        self.captured_attempts
    }
    /// Uncacheable captures for this unit.
    #[must_use]
    pub const fn uncacheable_attempts(&self) -> u64 {
        self.uncacheable_attempts
    }
    /// Checked phase totals, sorted by original phase order.
    #[must_use]
    pub fn phases(&self) -> &[PhaseWork] {
        &self.phases
    }
    /// Checked costs, or unavailable because this unit had no attempts.
    #[must_use]
    pub const fn work(&self) -> Option<Work> {
        self.work
    }
}

/// Exact-run immutable observation, constructed only by successful parsing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    summary: Summary,
    units: Vec<Unit>,
}

impl Report {
    /// Exact original analysis session.
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.summary.run_id
    }
    /// Input-work scope; generated outputs are excluded.
    #[must_use]
    pub const fn scope(&self) -> &'static str {
        SCOPE
    }
    /// Original availability, independent of build outcome.
    #[must_use]
    pub const fn availability(&self) -> Availability {
        self.summary.availability
    }
    /// Validated graph and capture denominators.
    #[must_use]
    pub fn denominators(&self) -> &Denominators {
        &self.summary.denominators
    }
    /// Checked aggregate costs, or explicitly unavailable legacy costs.
    #[must_use]
    pub const fn totals(&self) -> Option<Work> {
        self.summary.totals
    }
    /// Every original unit row, including rows without measured attempts.
    #[must_use]
    pub fn units(&self) -> &[Unit] {
        &self.units
    }
    /// Bounded projection suitable for a host observation record.
    #[must_use]
    pub fn summary(&self) -> Summary {
        self.summary.clone()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    schema: String,
    run_id: String,
    scope: String,
    availability: String,
    #[serde(deserialize_with = "nullable")]
    unavailable_reason: Option<String>,
    build_outcome: String,
    #[serde(deserialize_with = "object")]
    denominators: Denominators,
    #[serde(deserialize_with = "units")]
    units: Vec<Unit>,
    #[serde(deserialize_with = "nullable_object")]
    totals: Option<Work>,
}

/// Parse one exact-run report, refusing all malformed or inconsistent evidence.
/// # Errors
/// Returns `InvalidData` for oversized, malformed, duplicated, unsupported,
/// truncated, foreign-run or nonconserving reports. No partial report is returned.
pub fn parse(bytes: &[u8], expected_run: &str) -> io::Result<Report> {
    if bytes.is_empty() || bytes.len() > MAX_REPORT_BYTES || !valid_run(expected_run) {
        return Err(bad());
    }
    // Typed maps reject duplicate and unknown fields at every object depth.
    // Unlike parsing into Value first, no duplicate evidence is overwritten.
    let wire = aterm_json::from_slice::<Object<Wire>>(bytes)
        .map_err(|_| bad())?
        .0;
    if wire.schema != SCHEMA
        || wire.scope != SCOPE
        || wire.run_id != expected_run
        || wire.build_outcome != "not-recorded"
    {
        return Err(bad());
    }
    let availability = match (
        wire.availability.as_str(),
        wire.unavailable_reason.as_deref(),
    ) {
        ("complete", None) => Availability::Complete,
        ("unavailable", Some("legacy-log")) => Availability::UnavailableLegacyLog,
        _ => return Err(bad()),
    };
    let summary = Summary {
        run_id: wire.run_id,
        availability,
        denominators: wire.denominators,
        totals: wire.totals,
    };
    summary.validate()?;
    let mut totals = Work::default();
    let mut d = Denominators::default();
    let mut previous = None;
    for unit in &wire.units {
        if previous.is_some_and(|index| index >= unit.index)
            || !text(&unit.package_id)
            || !text(&unit.target.name)
            || !text(&unit.target.kind)
            || !text(&unit.platform)
            || unit.features.iter().any(|feature| !text(feature))
        {
            return Err(bad());
        }
        previous = Some(unit.index);
        add(&mut d.registered_units, 1)?;
        if unit.mode == Mode::RunCustomBuild {
            add(&mut d.run_script_units, 1)?;
        }
        if unit.attempts == 0 {
            if unit.captured_attempts != 0
                || unit.uncacheable_attempts != 0
                || !unit.phases.is_empty()
                || unit.work.is_some()
            {
                return Err(bad());
            }
            add(&mut d.units_without_attempts, 1)?;
            continue;
        }
        if availability != Availability::Complete
            || unit.mode != Mode::RunCustomBuild
            || unit.target.kind != "build-script"
            || unit.attempts > MAX_ATTEMPTS
            || unit
                .captured_attempts
                .checked_add(unit.uncacheable_attempts)
                != Some(unit.attempts)
        {
            return Err(bad());
        }
        let mut attempts = 0;
        let mut captured = 0;
        let mut uncacheable = 0;
        let mut work = Work::default();
        let mut prior_phase = None;
        for phase in &unit.phases {
            if prior_phase.is_some_and(|prior| prior >= phase.phase)
                || phase.attempts == 0
                || phase
                    .captured_attempts
                    .checked_add(phase.uncacheable_attempts)
                    != Some(phase.attempts)
            {
                return Err(bad());
            }
            prior_phase = Some(phase.phase);
            add(&mut attempts, phase.attempts)?;
            add(&mut captured, phase.captured_attempts)?;
            add(&mut uncacheable, phase.uncacheable_attempts)?;
            work.add(phase.work)?;
        }
        if (attempts, captured, uncacheable)
            != (
                unit.attempts,
                unit.captured_attempts,
                unit.uncacheable_attempts,
            )
            || unit.work != Some(work)
        {
            return Err(bad());
        }
        add(&mut d.observed_units, 1)?;
        add(&mut d.attempts_started, attempts)?;
        add(&mut d.attempts_finished, attempts)?;
        add(&mut d.captured_attempts, captured)?;
        add(&mut d.uncacheable_attempts, uncacheable)?;
        totals.add(work)?;
    }
    if d != summary.denominators
        || (availability == Availability::Complete && summary.totals != Some(totals))
    {
        return Err(bad());
    }
    Ok(Report {
        summary,
        units: wire.units,
    })
}

fn bad() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid compiler cache-work report",
    )
}

fn add(total: &mut u64, value: u64) -> io::Result<()> {
    *total = total.checked_add(value).ok_or_else(bad)?;
    Ok(())
}

fn text(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_TEXT_BYTES && !value.chars().any(char::is_control)
}

fn valid_run(value: &str) -> bool {
    let b = value.as_bytes();
    b.len() == 36
        && b[8] == b'T'
        && b[18] == b'Z'
        && b[19] == b'-'
        && b[..8].iter().chain(&b[9..18]).all(u8::is_ascii_digit)
        && b[20..]
            .iter()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
}

// A custom nullable field remains required by derive even when its value is
// null; an absent field cannot silently become an unobserved measurement.
fn nullable<'de, D: Deserializer<'de>, T: Deserialize<'de>>(de: D) -> Result<Option<T>, D::Error> {
    Option::<T>::deserialize(de)
}

// Serde also permits a derived struct's positional sequence representation.
// This wire contract requires JSON objects at every record depth instead.
struct Object<T>(T);

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Object<T> {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        struct ObjectVisitor<T>(PhantomData<T>);
        impl<'de, T: Deserialize<'de>> de::Visitor<'de> for ObjectVisitor<T> {
            type Value = Object<T>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a report object")
            }
            fn visit_map<A: de::MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                T::deserialize(de::value::MapAccessDeserializer::new(map)).map(Object)
            }
        }
        de.deserialize_map(ObjectVisitor(PhantomData))
    }
}

fn object<'de, D: Deserializer<'de>, T: Deserialize<'de>>(de: D) -> Result<T, D::Error> {
    Object::<T>::deserialize(de).map(|value| value.0)
}

fn nullable_object<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    de: D,
) -> Result<Option<T>, D::Error> {
    Option::<Object<T>>::deserialize(de).map(|value| value.map(|value| value.0))
}

fn bounded<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    de: D,
    max: usize,
) -> Result<Vec<T>, D::Error> {
    struct Bounded<T> {
        max: usize,
        marker: PhantomData<T>,
    }
    impl<'de, T: Deserialize<'de>> de::Visitor<'de> for Bounded<T> {
        type Value = Vec<T>;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a bounded array")
        }
        fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<T>, A::Error> {
            let mut values = Vec::new();
            while let Some(value) = seq.next_element()? {
                if values.len() >= self.max {
                    return Err(de::Error::custom("array exceeds report bound"));
                }
                values.push(value);
            }
            Ok(values)
        }
    }
    de.deserialize_seq(Bounded {
        max,
        marker: PhantomData,
    })
}

fn units<'de, D: Deserializer<'de>>(de: D) -> Result<Vec<Unit>, D::Error> {
    bounded::<D, Object<Unit>>(de, MAX_UNITS)
        .map(|values| values.into_iter().map(|value| value.0).collect())
}
fn phases<'de, D: Deserializer<'de>>(de: D) -> Result<Vec<PhaseWork>, D::Error> {
    bounded::<D, Object<PhaseWork>>(de, 3)
        .map(|values| values.into_iter().map(|value| value.0).collect())
}
fn features<'de, D: Deserializer<'de>>(de: D) -> Result<Vec<String>, D::Error> {
    bounded(de, MAX_FEATURES)
}

#[cfg(test)]
#[path = "cache_work_tests.rs"]
mod tests;
