// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Signed successor handoff-policy TOML parser, range resolver, and release-cut validator.

use std::path::Path;

/// Supported handoff-policy schema version.
pub const SCHEMA: i64 = 1;

/// Bundle-relative path to the signed handoff policy inside a release `.app`.
pub const BUNDLE_PATH: &str = "Contents/Resources/aterm-handoff-policy.toml";

/// Workspace-relative source path copied by the release cutter.
pub const SOURCE_PATH: &str = "publish/handoff-policy.toml";

/// Maximum byte size accepted when reading a handoff policy file.
pub const MAX_POLICY_BYTES: u64 = 16 * 1024;

/// First release build number (`v0.97.0`) that reads and follows a successor handoff policy.
pub const FIRST_POLICY_FOLLOWING_BUILD: u64 = 1_790_585_188;

/// `[[ranges]]`: the rescues, one table each ([`Rescue`]).
const RANGES_KEY: &str = "ranges";

/// `[[retired]]`: the cutter's record of the rescues retired ([`Retired`]).
const RETIRED_KEY: &str = "retired";

const KNOWN_KEYS: [&str; 6] = [
    "schema",
    "applies_to_producers",
    "carry",
    "park_quiet_gate_at_land",
    RANGES_KEY,
    RETIRED_KEY,
];

const RESCUE_KEYS: [&str; 4] = ["producers", "carry", "park_quiet_gate_at_land", "reason"];

const RETIRED_KEYS: [&str; 3] = ["producers", "reason", "date"];

const RESERVED_KEYS: [&str; 1] = ["seamless"];

/// Legacy `retire` key refused by the cutter in favor of `[[retired]]`.
const RETIRE_KEY: &str = "retire";

/// Maximum screen-carry fidelity rung allowed for a producer (`Full < Visible < Repaint`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CarryCeiling {
    /// Producer's normal capture ladder, including scrollback.
    Full,
    /// Visible screen and scalars only, excluding scrollback.
    Visible,
    /// Blank canonical screen redrawn by the foreground program.
    Repaint,
}

impl CarryCeiling {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "full" => Some(Self::Full),
            "visible" => Some(Self::Visible),
            "repaint" => Some(Self::Repaint),
            _ => None,
        }
    }

    /// Policy key spelling for this ceiling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Visible => "visible",
            Self::Repaint => "repaint",
        }
    }

    /// Whether a park under this ceiling carries scrollback (`Full` only).
    #[must_use]
    pub const fn carries_scrollback(self) -> bool {
        matches!(self, Self::Full)
    }
}

/// How the automatic update lane's `Land` phase treats queued PTY master output.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LandQuietGate {
    /// Wait for queued output to drain.
    Strict,
    /// Park immediately over queued output.
    Relaxed,
}

impl LandQuietGate {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "strict" => Some(Self::Strict),
            "relaxed" => Some(Self::Relaxed),
            _ => None,
        }
    }

    /// Policy key spelling for this gate mode.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Strict => "strict",
            Self::Relaxed => "relaxed",
        }
    }
}

/// Inclusive producer build range (`applies_to_producers`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ProducerRange {
    pub min_build: u64,
    pub max_build: u64,
}

impl ProducerRange {
    /// Whether `build` lies within `[min_build, max_build]`.
    #[must_use]
    pub const fn contains(self, build: u64) -> bool {
        self.min_build <= build && build <= self.max_build
    }
}

/// Schema-v1 handoff policy knobs and optional producer build range.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct HandoffPolicy {
    pub applies_to_producers: Option<ProducerRange>,
    pub carry: Option<CarryCeiling>,
    pub park_quiet_gate_at_land: Option<LandQuietGate>,
}

impl HandoffPolicy {
    /// Parse top-level schema-v1 policy keys.
    pub fn parse(text: &str) -> Result<Self, String> {
        Self::of_table(&table_of(text)?, false)
    }

    fn of_table(table: &aterm_toml::Table, strict: bool) -> Result<Self, String> {
        if strict {
            if let Some(key) = table
                .keys()
                .find(|key| RESERVED_KEYS.contains(&key.as_str()))
            {
                return Err(format!(
                    "`{key}` is reserved: no build implements it, because it would replace the \
                     in-session handover with a relaunch offer"
                ));
            }
            if table.get(RETIRE_KEY).is_some() {
                return Err(format!(
                    "`{RETIRE_KEY}` is now a `[[retired]]` table: {}",
                    Retired::SHAPE
                ));
            }
            if let Some(key) = table.keys().find(|key| !KNOWN_KEYS.contains(&key.as_str())) {
                return Err(format!(
                    "unknown key `{key}` (schema {SCHEMA} defines {})",
                    KNOWN_KEYS.join(", ")
                ));
            }
        }
        match table.get("schema").map(aterm_toml::Value::as_integer) {
            Some(Some(SCHEMA)) => {}
            Some(Some(other)) => {
                return Err(format!(
                    "schema {other}, and this build reads schema {SCHEMA} only"
                ));
            }
            Some(None) => return Err("`schema` is not an integer".to_string()),
            None => return Err(format!("no `schema = {SCHEMA}` line")),
        }
        let applies_to_producers = match table.get("applies_to_producers") {
            None => None,
            Some(value) => Some(parse_range("applies_to_producers", value)?),
        };
        let (carry, park_quiet_gate_at_land) = knobs_of(table)?;
        Ok(Self {
            applies_to_producers,
            carry,
            park_quiet_gate_at_land,
        })
    }

    /// Whether a producer at `build` is covered by this policy.
    #[must_use]
    pub fn applies_to(&self, build: u64) -> bool {
        self.applies_to_producers
            .is_none_or(|range| range.contains(build))
    }

    /// Whether this policy sets any non-default constraint.
    #[must_use]
    pub fn asks_anything(&self) -> bool {
        self.carry.is_some_and(|carry| carry != CarryCeiling::Full) || self.relaxes_land_gate()
    }

    /// Effective [`CarryCeiling`] (defaults to [`CarryCeiling::Full`]).
    #[must_use]
    pub fn carry_ceiling(&self) -> CarryCeiling {
        self.carry.unwrap_or(CarryCeiling::Full)
    }

    /// Whether the `Land` phase may park over queued output immediately.
    #[must_use]
    pub fn relaxes_land_gate(&self) -> bool {
        self.park_quiet_gate_at_land == Some(LandQuietGate::Relaxed)
    }

    fn combined<'a>(policies: impl IntoIterator<Item = &'a Self>, range: ProducerRange) -> Self {
        let mut out = Self {
            applies_to_producers: Some(range),
            carry: None,
            park_quiet_gate_at_land: None,
        };
        for policy in policies {
            out.carry = out.carry.max(policy.carry);
            out.park_quiet_gate_at_land =
                match (out.park_quiet_gate_at_land, policy.park_quiet_gate_at_land) {
                    (Some(LandQuietGate::Relaxed), _) | (_, Some(LandQuietGate::Relaxed)) => {
                        Some(LandQuietGate::Relaxed)
                    }
                    (Some(LandQuietGate::Strict), _) | (_, Some(LandQuietGate::Strict)) => {
                        Some(LandQuietGate::Strict)
                    }
                    (None, None) => None,
                };
        }
        out
    }

    /// Verify that a non-empty policy covers at least one build in `handing_over`.
    /// # Errors
    /// Returns a refusal message if the policy range misses builds or exceeds the newest build.
    pub fn names_producers(&self, handing_over: &[u64]) -> Result<(), String> {
        if !self.asks_anything() {
            return Ok(());
        }
        let Some(&newest) = handing_over.iter().max() else {
            return Err(
                "no build can hand over to the release that seals it, so no producer would \
                 follow it"
                    .to_string(),
            );
        };
        if !handing_over
            .iter()
            .any(|&build| build >= FIRST_POLICY_FOLLOWING_BUILD && self.applies_to(build))
        {
            return Err(format!(
                "it applies to none of the builds that can hand over to the release that seals \
                 it and follow a policy (from {FIRST_POLICY_FOLLOWING_BUILD} on; an older \
                 producer hands over without reading one), so no producer would follow it"
            ));
        }
        if newest
            .checked_add(1)
            .is_some_and(|next| self.applies_to(next))
        {
            return Err(format!(
                "it applies to builds after {newest}, the newest that can hand over to the \
                 release that seals it: every later release must keep it, so the producers of \
                 releases that never had the bug would follow it"
            ));
        }
        Ok(())
    }

    fn as_lines(&self) -> String {
        let mut lines = Vec::new();
        if let Some(range) = self.applies_to_producers {
            lines.push(format!(
                "`applies_to_producers = [{}, {}]`",
                range.min_build, range.max_build
            ));
        }
        if self.carry_ceiling() != CarryCeiling::Full {
            lines.push(format!("`carry = \"{}\"`", self.carry_ceiling().as_str()));
        }
        if self.relaxes_land_gate() {
            lines.push("`park_quiet_gate_at_land = \"relaxed\"`".to_string());
        }
        lines.join(", ")
    }
}

impl std::fmt::Display for HandoffPolicy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut said = Vec::new();
        if let Some(range) = self.applies_to_producers {
            said.push(format!(
                "applies_to_producers=[{}, {}]",
                range.min_build, range.max_build
            ));
        }
        if let Some(carry) = self.carry {
            said.push(format!("carry={}", carry.as_str()));
        }
        if let Some(gate) = self.park_quiet_gate_at_land {
            said.push(format!("park_quiet_gate_at_land={}", gate.as_str()));
        }
        if said.is_empty() {
            formatter.write_str("no knobs set")
        } else {
            formatter.write_str(&said.join(" "))
        }
    }
}

/// Single `[[ranges]]` rescue entry with its target producer range, knobs, and reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rescue {
    /// Producer build range and policy knobs.
    pub policy: HandoffPolicy,
    /// Human-readable reason required by the release cutter.
    pub reason: String,
}

impl Rescue {
    fn list_of(value: &aterm_toml::Value, strict: bool) -> Result<Vec<Self>, String> {
        let tables = value
            .as_array()
            .ok_or_else(|| format!("`{RANGES_KEY}` is {value}, not a list of [[ranges]] tables"))?;
        let mut rescues = Vec::with_capacity(tables.len());
        for (index, table) in tables.iter().enumerate() {
            let entry = index + 1;
            let table = table
                .as_table()
                .ok_or_else(|| format!("[[ranges]] table {entry} is {table}, not a table"))?;
            let at = |why: String| format!("[[ranges]] table {entry}: {why}");
            if strict
                && let Some(key) = table
                    .keys()
                    .find(|key| !RESCUE_KEYS.contains(&key.as_str()))
            {
                return Err(at(format!(
                    "unknown key `{key}` (a rescue holds {})",
                    RESCUE_KEYS.join(", ")
                )));
            }
            let producers = parse_range(
                "producers",
                table
                    .get("producers")
                    .ok_or_else(|| at("no `producers = [min_build, max_build]`".to_string()))?,
            )
            .map_err(at)?;
            let (carry, park_quiet_gate_at_land) = knobs_of(table).map_err(at)?;
            let policy = HandoffPolicy {
                applies_to_producers: Some(producers),
                carry,
                park_quiet_gate_at_land,
            };
            let reason = table
                .get("reason")
                .and_then(aterm_toml::Value::as_str)
                .map_or("", str::trim)
                .to_string();
            if strict && reason.is_empty() {
                return Err(at(
                    "no `reason`: say which builds' bug it routes around, so a later release \
                     can tell when to retire it"
                        .to_string(),
                ));
            }
            if strict && !policy.asks_anything() {
                return Err(at(format!(
                    "{policy} asks nothing: set `carry = \"visible\"` or `\"repaint\"`, or \
                     `park_quiet_gate_at_land = \"relaxed\"`, or delete the table"
                )));
            }
            rescues.push(Self { policy, reason });
        }
        Ok(rescues)
    }
}

/// Parsed state of a policy file's `[[ranges]]` list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Ranges {
    /// No `ranges` key present; top-level keys define the entire policy.
    #[default]
    Absent,
    /// Parsed `[[ranges]]` rescue entries.
    Listed(Vec<Rescue>),
    /// Malformed `[[ranges]]` entry; producer falls back to top-level hull keys.
    Unreadable(String),
}

/// Parsed policy file containing both top-level legacy/hull keys and `[[ranges]]`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PolicyFile {
    /// Top-level policy keys ([`HandoffPolicy::parse`]).
    pub legacy: HandoffPolicy,
    /// Parsed `[[ranges]]` state.
    pub ranges: Ranges,
}

impl PolicyFile {
    /// Parse a policy file as read by a producer supporting `[[ranges]]`.
    pub fn parse(text: &str) -> Result<Self, String> {
        let table = table_of(text)?;
        let legacy = HandoffPolicy::of_table(&table, false)?;
        let ranges = match table.get(RANGES_KEY) {
            None => Ranges::Absent,
            Some(value) => match Rescue::list_of(value, false) {
                Ok(rescues) => Ranges::Listed(rescues),
                Err(why) => Ranges::Unreadable(why),
            },
        };
        Ok(Self { legacy, ranges })
    }

    /// Strictly parse and validate a checked-in policy file for release cutting.
    /// # Errors
    /// Returns a refusal string on unknown keys, invalid entries, hull mismatch, or overlap.
    pub fn parse_for_cut(text: &str) -> Result<Self, String> {
        let table = table_of(text)?;
        let legacy = HandoffPolicy::of_table(&table, true)?;
        let retired = Retired::list_of(&table)?;
        let Some(value) = table.get(RANGES_KEY) else {
            if legacy.asks_anything() {
                return Err(format!(
                    "a knob outside `[[ranges]]` ({legacy}): write each rescue as a `[[ranges]]` \
                     table naming its `producers` and its `reason`, and the top-level keys as \
                     the hull of them all, which builds before `[[ranges]]` read alone"
                ));
            }
            return Ok(Self {
                legacy,
                ranges: Ranges::Absent,
            });
        };
        let rescues = Rescue::list_of(value, true)?;
        let Some(hull) = Self::hull(&rescues) else {
            return Err(format!(
                "`{RANGES_KEY}` lists no rescue: delete the key, or add a [[ranges]] table"
            ));
        };
        if (
            legacy.applies_to_producers,
            legacy.carry_ceiling(),
            legacy.relaxes_land_gate(),
        ) != (
            hull.applies_to_producers,
            hull.carry_ceiling(),
            hull.relaxes_land_gate(),
        ) {
            return Err(format!(
                "the top-level keys ({legacy}) are not the hull of `[[ranges]]`, and every build \
                 before `[[ranges]]` follows them alone: write {} above the first [[ranges]] \
                 table, and no other top-level knob",
                hull.as_lines()
            ));
        }
        if let Some(retired) = retired.iter().find(|retired| {
            rescues
                .iter()
                .any(|rescue| rescue.policy.applies_to_producers == Some(retired.producers))
        }) {
            return Err(format!(
                "builds [{}, {}] are both rescued under `[[ranges]]` and listed under \
                 `[[retired]]`: delete one of them",
                retired.producers.min_build, retired.producers.max_build
            ));
        }
        Ok(Self {
            legacy,
            ranges: Ranges::Listed(rescues),
        })
    }

    /// Compute the bounding hull [`HandoffPolicy`] over `rescues`.
    #[must_use]
    pub fn hull(rescues: &[Rescue]) -> Option<HandoffPolicy> {
        let ranges = rescues
            .iter()
            .filter_map(|rescue| rescue.policy.applies_to_producers);
        let range = ranges.reduce(|hull, range| ProducerRange {
            min_build: hull.min_build.min(range.min_build),
            max_build: hull.max_build.max(range.max_build),
        })?;
        Some(HandoffPolicy::combined(
            rescues.iter().map(|rescue| &rescue.policy),
            range,
        ))
    }

    /// Resolve the effective [`HandoffPolicy`] for `build`.
    #[must_use]
    pub fn for_build(&self, build: u64) -> Option<HandoffPolicy> {
        match &self.ranges {
            Ranges::Listed(rescues) => {
                let naming: Vec<&HandoffPolicy> = rescues
                    .iter()
                    .map(|rescue| &rescue.policy)
                    .filter(|policy| policy.applies_to(build))
                    .collect();
                let shared = naming
                    .iter()
                    .filter_map(|policy| policy.applies_to_producers)
                    .reduce(|shared, range| ProducerRange {
                        min_build: shared.min_build.max(range.min_build),
                        max_build: shared.max_build.min(range.max_build),
                    })?;
                Some(HandoffPolicy::combined(naming, shared))
            }
            Ranges::Absent | Ranges::Unreadable(_) => {
                self.legacy.applies_to(build).then_some(self.legacy)
            }
        }
    }

    /// Return the active [`Rescue`] entries in the file.
    /// # Errors
    /// Returns an error if `[[ranges]]` is [`Ranges::Unreadable`].
    pub fn rescues(&self) -> Result<Vec<Rescue>, String> {
        match &self.ranges {
            Ranges::Listed(rescues) => Ok(rescues.clone()),
            Ranges::Absent => Ok(if self.legacy.asks_anything() {
                vec![Rescue {
                    policy: self.legacy,
                    reason: String::new(),
                }]
            } else {
                Vec::new()
            }),
            Ranges::Unreadable(why) => Err(why.clone()),
        }
    }

    /// Validate [`HandoffPolicy::names_producers`] across every rescue in the file.
    /// # Errors
    /// Returns the first rescue whose producer range fails validation.
    pub fn names_producers(&self, handing_over: &[u64]) -> Result<(), String> {
        for rescue in self.rescues()? {
            rescue.policy.names_producers(handing_over).map_err(|why| {
                format!(
                    "{}: {why}",
                    rescue.policy.applies_to_producers.map_or(
                        "its rescue for every build".to_string(),
                        |range| format!(
                            "its rescue for builds [{}, {}]",
                            range.min_build, range.max_build
                        )
                    )
                )
            })?;
        }
        Ok(())
    }

    /// Verify that every build and constraint in an earlier `sealed` rescue is still covered.
    /// # Errors
    /// Returns a refusal naming the first build in `sealed` whose coverage was dropped or weakened.
    pub fn keeps(&self, sealed: &HandoffPolicy) -> Result<(), String> {
        if !sealed.asks_anything() {
            return Ok(());
        }
        let named = sealed.applies_to_producers.unwrap_or(ProducerRange {
            min_build: 0,
            max_build: u64::MAX,
        });
        let read: Vec<ProducerRange> = match &self.ranges {
            Ranges::Listed(rescues) => rescues
                .iter()
                .filter_map(|rescue| rescue.policy.applies_to_producers)
                .collect(),
            Ranges::Absent | Ranges::Unreadable(_) => {
                self.legacy.applies_to_producers.into_iter().collect()
            }
        };
        let mut stretches = vec![named.min_build];
        for range in read {
            stretches.extend(
                [Some(range.min_build), range.max_build.checked_add(1)]
                    .into_iter()
                    .flatten()
                    .filter(|&build| named.contains(build)),
            );
        }
        stretches.sort_unstable();
        stretches.dedup();
        for build in stretches {
            let Some(now) = self.for_build(build) else {
                return Err(format!(
                    "it no longer applies to build {build}, which the earlier policy named"
                ));
            };
            if now.carry_ceiling() < sealed.carry_ceiling() {
                return Err(format!(
                    "it raises `carry` for build {build} from \"{}\" to \"{}\"",
                    sealed.carry_ceiling().as_str(),
                    now.carry_ceiling().as_str()
                ));
            }
            if sealed.relaxes_land_gate() && !now.relaxes_land_gate() {
                return Err(format!(
                    "it drops `park_quiet_gate_at_land = \"relaxed\"` for build {build}"
                ));
            }
        }
        Ok(())
    }
}

impl std::fmt::Display for PolicyFile {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.ranges {
            Ranges::Absent => write!(formatter, "{}", self.legacy),
            Ranges::Listed(rescues) if rescues.is_empty() => {
                formatter.write_str("no rescue listed")
            }
            Ranges::Listed(rescues) => {
                let said: Vec<String> = rescues
                    .iter()
                    .map(|rescue| {
                        if rescue.reason.is_empty() {
                            rescue.policy.to_string()
                        } else {
                            format!("{} ({})", rescue.policy, rescue.reason)
                        }
                    })
                    .collect();
                formatter.write_str(&said.join("; "))
            }
            Ranges::Unreadable(why) => write!(
                formatter,
                "{} (a `[[ranges]]` list this build cannot read: {why})",
                self.legacy
            ),
        }
    }
}

/// Single `[[retired]]` entry recording a purposely retired producer rescue range.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Retired {
    /// Exact producer range of the retired rescue.
    pub producers: ProducerRange,
    /// Reason no build in `producers` still needs the rescue.
    pub reason: String,
    /// Retirement date (`YYYY-MM-DD`).
    pub date: String,
}

impl Retired {
    const SHAPE: &str = "`producers = [min_build, max_build]`, `reason = \"why no build in \
                         that range needs the rescue\"`, `date = \"YYYY-MM-DD\"`";

    /// Strictly parse the `[[retired]]` table list for release cutting.
    /// # Errors
    /// Returns an error if the TOML or any `[[retired]]` entry is invalid.
    pub fn parse_for_cut(text: &str) -> Result<Vec<Self>, String> {
        Self::list_of(&table_of(text)?)
    }

    fn list_of(table: &aterm_toml::Table) -> Result<Vec<Self>, String> {
        let Some(value) = table.get(RETIRED_KEY) else {
            return Ok(Vec::new());
        };
        let tables = value.as_array().ok_or_else(|| {
            format!(
                "`{RETIRED_KEY}` is {value}, not a list of [[retired]] tables, each {}",
                Self::SHAPE
            )
        })?;
        let mut retired = Vec::with_capacity(tables.len());
        for (index, table) in tables.iter().enumerate() {
            let wrong = || {
                format!(
                    "[[retired]] table {} is not {} alone",
                    index + 1,
                    Self::SHAPE
                )
            };
            let table = table.as_table().ok_or_else(wrong)?;
            if table.len() != RETIRED_KEYS.len()
                || table
                    .keys()
                    .any(|key| !RETIRED_KEYS.contains(&key.as_str()))
            {
                return Err(wrong());
            }
            let producers = parse_range("producers", table.get("producers").ok_or_else(wrong)?)
                .map_err(|_| wrong())?;
            let text = |key: &str| {
                table
                    .get(key)
                    .and_then(aterm_toml::Value::as_str)
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .map(str::to_string)
            };
            let reason = text("reason").ok_or_else(wrong)?;
            let date = text("date")
                .filter(|date| is_calendar_date(date))
                .ok_or_else(wrong)?;
            retired.push(Self {
                producers,
                reason,
                date,
            });
        }
        Ok(retired)
    }
}

fn is_calendar_date(text: &str) -> bool {
    let bytes = text.as_bytes();
    let digits = |range: std::ops::Range<usize>| {
        bytes
            .get(range)
            .filter(|part| part.iter().all(u8::is_ascii_digit))
            .and_then(|part| std::str::from_utf8(part).ok()?.parse::<u32>().ok())
    };
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && digits(0..4).is_some()
        && digits(5..7).is_some_and(|month| (1..=12).contains(&month))
        && digits(8..10).is_some_and(|day| (1..=31).contains(&day))
}

fn table_of(text: &str) -> Result<aterm_toml::Table, String> {
    match aterm_toml::from_str(text).map_err(|error| format!("not TOML: {error}"))? {
        aterm_toml::Value::Table(table) => Ok(table),
        _ => Err("not a TOML table".to_string()),
    }
}

fn knobs_of(
    table: &aterm_toml::Table,
) -> Result<(Option<CarryCeiling>, Option<LandQuietGate>), String> {
    let carry = match table.get("carry") {
        None => None,
        Some(value) => Some(
            value
                .as_str()
                .and_then(CarryCeiling::parse)
                .ok_or_else(|| {
                    format!("`carry` is {value}, not \"full\", \"visible\" or \"repaint\"")
                })?,
        ),
    };
    let park_quiet_gate_at_land =
        match table.get("park_quiet_gate_at_land") {
            None => None,
            Some(value) => Some(value.as_str().and_then(LandQuietGate::parse).ok_or_else(
                || format!("`park_quiet_gate_at_land` is {value}, not \"strict\" or \"relaxed\""),
            )?),
        };
    Ok((carry, park_quiet_gate_at_land))
}

fn parse_range(key: &str, value: &aterm_toml::Value) -> Result<ProducerRange, String> {
    let wrong = || format!("`{key}` is {value}, not [min_build, max_build]");
    let array = value.as_array().ok_or_else(wrong)?;
    let [min, max] = array.as_slice() else {
        return Err(wrong());
    };
    let build = |value: &aterm_toml::Value| {
        value
            .as_integer()
            .and_then(|build| u64::try_from(build).ok())
    };
    let (Some(min_build), Some(max_build)) = (build(min), build(max)) else {
        return Err(wrong());
    };
    if min_build > max_build {
        return Err(format!(
            "`{key}` is [{min_build}, {max_build}], an empty range"
        ));
    }
    Ok(ProducerRange {
        min_build,
        max_build,
    })
}

/// Outcome of reading a candidate bundle's handoff policy file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PolicyRead {
    /// Bundle carries no policy file.
    Absent,
    /// Successfully parsed schema-v1 [`PolicyFile`].
    Parsed(PolicyFile),
    /// Policy file was present but invalid and is ignored.
    Ignored(String),
    /// Transient OS/I/O error prevented reading the policy file.
    Unread(String),
}

/// Read and parse the handoff policy from candidate bundle `app_root` without following symlinks.
#[must_use]
pub fn read_from_bundle(app_root: &Path) -> PolicyRead {
    let path = app_root.join(BUNDLE_PATH);
    judge(&path, read_bounded(&path))
}

/// What [`read_bounded`]'s answer for the policy at `path` means.
fn judge(path: &Path, read: Result<Option<String>, ReadFailure>) -> PolicyRead {
    match read {
        Ok(None) => PolicyRead::Absent,
        Ok(Some(text)) => match PolicyFile::parse(&text) {
            Ok(file) => PolicyRead::Parsed(file),
            Err(why) => PolicyRead::Ignored(format!("{}: {why}", path.display())),
        },
        Err(ReadFailure::File(why)) => PolicyRead::Ignored(format!("{}: {why}", path.display())),
        Err(ReadFailure::Moment(why)) => PolicyRead::Unread(format!("{}: {why}", path.display())),
    }
}

/// Why [`read_bounded`] could not hand back the file's text.
enum ReadFailure {
    /// A fact about the file: not a regular file, too large, not UTF-8, or an error the same
    /// file would give again.
    File(String),
    /// A fact about this moment ([`is_momentary`]).
    Moment(String),
}

fn is_momentary(error: &std::io::Error) -> bool {
    use std::io::ErrorKind;
    if matches!(
        error.kind(),
        ErrorKind::Interrupted | ErrorKind::WouldBlock | ErrorKind::OutOfMemory
    ) {
        return true;
    }
    #[cfg(unix)]
    {
        matches!(
            error.raw_os_error(),
            Some(libc::EMFILE | libc::ENFILE | libc::ENOMEM | libc::ENOBUFS | libc::EIO)
        )
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// Whether `error` is a transient OS/I/O failure that should be retried on the next check.
#[must_use]
pub fn is_momentary_read_error(error: &std::io::Error) -> bool {
    is_momentary(error)
}

/// An I/O error of the read, sorted by [`is_momentary`].
fn unreadable(error: &std::io::Error) -> ReadFailure {
    let why = format!("unreadable: {error}");
    if is_momentary(error) {
        ReadFailure::Moment(why)
    } else {
        ReadFailure::File(why)
    }
}

fn read_bounded(path: &Path) -> Result<Option<String>, ReadFailure> {
    use std::io::Read as _;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        #[cfg(unix)]
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
            return Err(ReadFailure::File(
                "not a regular file (a symbolic link)".to_string(),
            ));
        }
        Err(error) => return Err(unreadable(&error)),
    };
    let metadata = file.metadata().map_err(|error| unreadable(&error))?;
    if !metadata.file_type().is_file() {
        return Err(ReadFailure::File("not a regular file".to_string()));
    }
    if metadata.len() > MAX_POLICY_BYTES {
        return Err(ReadFailure::File(format!(
            "{} bytes, over the {MAX_POLICY_BYTES}-byte bound",
            metadata.len()
        )));
    }
    let mut bytes = Vec::with_capacity(
        usize::try_from(metadata.len())
            .unwrap_or(0)
            .saturating_add(1),
    );
    file.take(MAX_POLICY_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| unreadable(&error))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_POLICY_BYTES {
        return Err(ReadFailure::File(format!(
            "over the {MAX_POLICY_BYTES}-byte bound"
        )));
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| ReadFailure::File("not UTF-8".to_string()))
}

/// Producer decision for a [`PolicyRead`] at `producer_build`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Adoption {
    /// No policy file or no active constraints.
    None,
    /// Active [`HandoffPolicy`] to follow for `producer_build`.
    Follow(HandoffPolicy),
    /// Policy file applies only to other producer builds.
    NotForThisBuild(PolicyFile),
    /// Policy file could not be followed.
    Ignored(String),
}

impl Adoption {
    /// Compute the [`Adoption`] decision for `read` at `producer_build`.
    #[must_use]
    pub fn of(read: &PolicyRead, producer_build: u64) -> Self {
        match read {
            PolicyRead::Absent => Self::None,
            PolicyRead::Ignored(why) => Self::Ignored(why.clone()),
            PolicyRead::Unread(why) => Self::Ignored(format!("not read just then: {why}")),
            PolicyRead::Parsed(file) => match file.for_build(producer_build) {
                None => Self::NotForThisBuild(file.clone()),
                Some(policy) if policy.asks_anything() => Self::Follow(policy),
                Some(_) => Self::None,
            },
        }
    }

    /// Return the active [`HandoffPolicy`] to follow, if any.
    #[must_use]
    pub const fn policy(&self) -> Option<HandoffPolicy> {
        match self {
            Self::Follow(policy) => Some(*policy),
            Self::None | Self::NotForThisBuild(_) | Self::Ignored(_) => None,
        }
    }
}

/// Compute [`Adoption::of`] and emit the corresponding diagnostic log line.
#[must_use]
pub fn adopt(read: &PolicyRead, producer_build: u64, candidate: &str) -> Option<HandoffPolicy> {
    if let PolicyRead::Parsed(PolicyFile {
        ranges: Ranges::Unreadable(why),
        ..
    }) = read
    {
        aterm_log::warn!(
            "update apply: {candidate}'s handoff policy lists rescues this build cannot read \
             ({why}); it follows the policy's top-level keys, as an older build does"
        );
    }
    let adoption = Adoption::of(read, producer_build);
    match &adoption {
        Adoption::None => {}
        Adoption::Follow(policy) => aterm_log::info!(
            "update apply: {candidate} carries a handoff policy for this build \
             ({producer_build}): {policy}; the next park follows it"
        ),
        Adoption::NotForThisBuild(policy) => aterm_log::info!(
            "update apply: {candidate} carries a handoff policy for other builds ({policy}); \
             this build ({producer_build}) ignores it"
        ),
        Adoption::Ignored(why) => aterm_log::warn!(
            "update apply: {candidate}'s handoff policy is ignored ({why}); the park runs as \
             this build ships"
        ),
    }
    adoption.policy()
}

#[cfg(test)]
#[path = "handoff_policy_tests.rs"]
mod tests;
