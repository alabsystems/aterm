// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE SUCCESSOR'S HANDOFF POLICY (plan P0-5): a small, signed TOML file inside a
//! release's `.app` that lets the NEWER build tell the OLDER one handing its sessions
//! over to be more conservative.
//!
//! WHY IT EXISTS. An in-session update is decided by the OUTGOING build's code — the
//! park, the screen capture, the quiet gate — and the outgoing build is the older
//! one, so a producer bug cannot be fixed into the release that suffers it. The
//! capture ladder (`aterm-gui`'s `seamless::carry_for_wire`) makes the producer total
//! over the screen content it can SEE; this file is the escape hatch for the bugs it
//! cannot see: a capture path that is slow or panics, a proof that mismatches
//! itself, a park gate that never opens. From the release that seals it, the
//! successor can ask every affected producer to carry less — and only while later
//! releases keep it: a producer reads the policy of the release it hands over to
//! alone, so one that skipped the rescuing release is covered only if the newest
//! release still names it (`docs/RELEASING.md`).
//!
//! WHERE IT LIVES. The release cutter copies the checked-in [`SOURCE_PATH`]
//! (`publish/handoff-policy.toml`) into the bundle at [`BUNDLE_PATH`] BEFORE it
//! signs the bundle, so the code signature seals it with everything else under
//! `Contents/`. The outgoing build reads it from the CANDIDATE bundle — the staged
//! update, or the installed bundle an activation execs — only AFTER that bundle has
//! passed the same codesign policy and sealed-identity check that authorizes it as
//! the successor, and over the same bytes.
//!
//! WHY THAT IS ENOUGH TRUST. The candidate bundle is about to be exec'd as the
//! successor with every right this process has: it will own the terminal, the PTYs
//! and the user's files. A file sealed by that same signature can say nothing that
//! code could not do anyway, and every knob below can only LOWER what the producer
//! carries or relax a gate only the producer applies. None can touch a consumer
//! check, raise a cap, change the adoption proof, or skip the verification that
//! admitted the bundle.
//!
//! # Schema v1
//!
//! ```toml
//! schema = 1                              # required; any other value: file ignored
//! applies_to_producers = [min, max]       # optional; inclusive build numbers of the
//!                                         # OUTGOING builds that must follow it
//!                                         # (absent: every producer)
//! carry = "full" | "visible" | "repaint"  # optional; the HIGHEST rung the producer
//!                                         # may carry a session at
//! park_quiet_gate_at_land = "strict" | "relaxed"   # optional
//!
//! [[ranges]]                              # optional, one table per rescue
//! producers = [min, max]                  #   required: the builds it rescues
//! carry = "…"                             #   optional, as above, for those builds
//! park_quiet_gate_at_land = "…"           #   optional, as above, for those builds
//! reason = "…"                            #   what it routes around (the cutter
//!                                         #   requires it)
//!
//! [[retired]]                             # cutter-only, one table per retirement
//! producers = [min, max]                  #   see [`Retired`]
//! reason = "…"
//! date = "YYYY-MM-DD"
//! ```
//!
//! SEVERAL RESCUES (r8c finding 504). A release
//! may have to rescue more than one range of older builds, each with its own
//! knobs, and every one of them stays while its builds may still be installed.
//! Each rescue is a `[[ranges]]` table. The TOP-LEVEL keys stay the one range
//! v0.97.0 and every build up to this reader's first follows ALONE (it ignores
//! `ranges` as a key it does not know), so beside `[[ranges]]` they are written as
//! the HULL of the list ([`PolicyFile::hull`]): the lowest `min` to the highest
//! `max`, the highest `carry`, and the `Land` gate relaxed if any rescue relaxes
//! it. Such a build therefore gets at least what its own rescue asks; a build in a
//! gap between two ranges carries less than it needs to, and a degrade is never a
//! refusal. A producer that reads `ranges` ([`PolicyFile::for_build`]) follows
//! every rescue whose range names its build, combined the same way, and nothing
//! when none names it; a list it cannot read (a value it does not know) leaves it
//! on the hull, as an older build.
//!
//! * `carry = "visible"` carries no scrollback; `"repaint"` carries every session
//!   as a blank canonical screen that its program redraws, and no scrollback
//!   either. "No scrollback" covers both ways a producer carries it — the newest
//!   lines inside the screen carry and the whole history in a sidecar beside it
//!   ([`CarryCeiling::carries_scrollback`]) — and the lines left behind are counted
//!   and said like any scrollback an update cannot carry. `"full"` is the
//!   producer's own ladder, as it ships. A producer still takes a LOWER rung than
//!   the ceiling whenever its own ladder needs one.
//! * `park_quiet_gate_at_land = "relaxed"` lets the automatic lane's `Land` phase
//!   park over output still queued on a master at once, instead of after the
//!   producer's own bounded wait. `"strict"` is the producer's gate as it ships (the
//!   same as leaving the key out). It never relaxes the session-death check or the
//!   capture's mid-sequence check.
//! * `seamless` is RESERVED. It would route an update to a relaunch offer instead
//!   of the in-session handover, and the owner's rulings on update wording forbid
//!   asking a person to do that, so no reader implements it; the cutter refuses a
//!   policy that sets it.
//!
//! READING RULES — a producer that cannot follow a file falls back to today's
//! behaviour, never to something stricter:
//!
//! * No file: no policy.
//! * Unknown keys are ignored, so a later release can ADD a key without a schema
//!   bump (`[[ranges]]` and `[[retired]]` were added so, and a `[[ranges]]` table's
//!   unknown keys are ignored the same way). Bump `schema` only when an existing
//!   key changes meaning; a producer ignores a schema it does not know rather than
//!   guessing.
//! * A file that is unreadable, larger than [`MAX_POLICY_BYTES`], not TOML, or that
//!   gives a known top-level key a value this reader does not know, is ignored
//!   whole — the caller logs one WARN naming why. A `ranges` list it cannot read
//!   is not: the producer follows the top-level keys, the hull every older build
//!   follows, and the caller logs one WARN naming why.
//! * A policy that names no range holding the running build is not for it and is
//!   ignored.
//!
//! The CUTTER reads the checked-in file strictly ([`PolicyFile::parse_for_cut`]):
//! an unknown key there is a typo that would ship as a policy nobody follows, a
//! knob outside `[[ranges]]` has no reason and no range of its own, a rescue with
//! no reason cannot be judged for retirement later, and top-level keys that are
//! not the hull of the list would leave some older build less protected than its
//! rescue asks, so each refuses the cut before a build number is claimed. It
//! refuses a range that names no build able to hand over to the release and
//! follow a policy ([`FIRST_POLICY_FOLLOWING_BUILD`] on), or reaches past the
//! newest one ([`PolicyFile::names_producers`]), and a file that
//! drops a rescue the previous release sealed ([`PolicyFile::keeps`]), unless a
//! `[[retired]]` entry names that rescue's range with a reason and a date
//! ([`Retired`], cutter-only: producers ignore the key).

use std::path::Path;

/// The one schema this reader implements.
pub const SCHEMA: i64 = 1;

/// Where the policy sits inside a release `.app`, relative to the bundle root.
/// Under `Contents/`, so the bundle's code signature seals it.
pub const BUNDLE_PATH: &str = "Contents/Resources/aterm-handoff-policy.toml";

/// The checked-in source the release cutter copies, relative to the workspace root.
pub const SOURCE_PATH: &str = "publish/handoff-policy.toml";

/// The largest policy file a reader admits. Schema v1 is a few keys and a short
/// list of rescues and retirements; the bound only keeps a hostile or corrupt file
/// from costing a read of arbitrary size.
pub const MAX_POLICY_BYTES: u64 = 16 * 1024;

/// v0.97.0's build (`RELEASES.ledger`): the first release whose producer reads
/// and FOLLOWS a successor's policy (75f60b08c, which every release since
/// holds; v0.95.0, the release before it, was cut from a commit without it).
/// Every later cut holds it too: that commit also added [`SOURCE_PATH`], and the
/// cutter refuses a tree without the file. A producer older than this build
/// hands its sessions over without looking for the file: the successor reads a
/// loss such a producer reports as its own, whatever a policy asked, and the
/// cutter refuses a rescue that names only such builds, which no producer would
/// follow ([`HandoffPolicy::names_producers`]).
pub const FIRST_POLICY_FOLLOWING_BUILD: u64 = 1_790_585_188;

/// `[[ranges]]`: the rescues, one table each ([`Rescue`]).
const RANGES_KEY: &str = "ranges";

/// `[[retired]]`: the cutter's record of the rescues retired ([`Retired`]); a
/// producer ignores it like any key it does not know.
const RETIRED_KEY: &str = "retired";

/// Every top-level key schema v1 defines.
const KNOWN_KEYS: [&str; 6] = [
    "schema",
    "applies_to_producers",
    "carry",
    "park_quiet_gate_at_land",
    RANGES_KEY,
    RETIRED_KEY,
];

/// Every key a `[[ranges]]` table defines.
const RESCUE_KEYS: [&str; 4] = ["producers", "carry", "park_quiet_gate_at_land", "reason"];

/// Every key a `[[retired]]` table holds, all of them required.
const RETIRED_KEYS: [&str; 3] = ["producers", "reason", "date"];

/// Keys no reader implements, refused by the cutter by name.
const RESERVED_KEYS: [&str; 1] = ["seamless"];

/// The `retire = { producers, reason }` spelling `[[retired]]` replaced, refused by
/// the cutter with the spelling that replaced it.
const RETIRE_KEY: &str = "retire";

/// The highest rung a producer may carry a session's screen at, from most faithful
/// to least. Ordered in that direction — `Full < Visible < Repaint` — so the `max`
/// of two ceilings is the more conservative one, and a ceiling GREATER than the one
/// a capture was taken under asks for less than that capture carried.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CarryCeiling {
    /// The producer's own ladder, scrollback included.
    Full,
    /// The visible screen and its scalars, never scrollback.
    Visible,
    /// A blank, canonical screen its program redraws.
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

    /// The key's spelling of this value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Visible => "visible",
            Self::Repaint => "repaint",
        }
    }

    /// Whether a park under this ceiling carries ANY of a session's scrollback —
    /// `Full` only. `"visible"` and `"repaint"` promise "never scrollback", and the
    /// producer carries scrollback two ways: inside the screen carry (its bounded
    /// newest lines) and beside it (`aterm-gui`'s history carry, the whole history
    /// in a sidecar file). Both read this one answer, so neither can carry what the
    /// other withholds.
    #[must_use]
    pub const fn carries_scrollback(self) -> bool {
        matches!(self, Self::Full)
    }
}

/// How the automatic lane's `Land` phase treats output queued on a master.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LandQuietGate {
    /// The producer's gate as it ships.
    Strict,
    /// Park over queued output at once.
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

    /// The key's spelling of this value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Strict => "strict",
            Self::Relaxed => "relaxed",
        }
    }
}

/// `applies_to_producers`: the OUTGOING builds a policy is for, both ends inclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ProducerRange {
    pub min_build: u64,
    pub max_build: u64,
}

impl ProducerRange {
    /// Whether `build` is inside the range.
    #[must_use]
    pub const fn contains(self, build: u64) -> bool {
        self.min_build <= build && build <= self.max_build
    }
}

/// One schema-v1 policy as ONE producer follows it: the builds it is for and its
/// knobs. Every field is optional; the default is no policy. A file's top-level
/// keys read as one ([`Self::parse`]), and so do each of its `[[ranges]]` tables
/// ([`Rescue`]) and what a producer resolves from them ([`PolicyFile::for_build`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct HandoffPolicy {
    pub applies_to_producers: Option<ProducerRange>,
    pub carry: Option<CarryCeiling>,
    pub park_quiet_gate_at_land: Option<LandQuietGate>,
}

impl HandoffPolicy {
    /// Parse a file's TOP-LEVEL keys the way every producer from v0.97.0 up to the
    /// first that reads `[[ranges]]` reads the whole file — this is that code, and
    /// beside `[[ranges]]` it answers the list's hull. Unknown keys are ignored (a
    /// later release may add keys), and anything this reader cannot interpret is an
    /// `Err` naming why — which the caller treats as no policy at all.
    pub fn parse(text: &str) -> Result<Self, String> {
        Self::of_table(&table_of(text)?, false)
    }

    /// The top-level keys of `table`; `strict` is the cutter's read, which also
    /// refuses a reserved key and any key schema v1 does not define.
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

    /// Whether a producer running `build` must follow this policy: every build when
    /// no range is given, else the builds inside it.
    #[must_use]
    pub fn applies_to(&self, build: u64) -> bool {
        self.applies_to_producers
            .is_none_or(|range| range.contains(build))
    }

    /// Whether the policy asks a producer for anything at all. The checked-in file
    /// is empty by default, and an empty policy is no policy.
    #[must_use]
    pub fn asks_anything(&self) -> bool {
        self.carry.is_some_and(|carry| carry != CarryCeiling::Full) || self.relaxes_land_gate()
    }

    /// The highest rung the producer may carry at: [`CarryCeiling::Full`] (its own
    /// ladder) when the policy does not say.
    #[must_use]
    pub fn carry_ceiling(&self) -> CarryCeiling {
        self.carry.unwrap_or(CarryCeiling::Full)
    }

    /// Whether the `Land` phase may park over queued output at once.
    #[must_use]
    pub fn relaxes_land_gate(&self) -> bool {
        self.park_quiet_gate_at_land == Some(LandQuietGate::Relaxed)
    }

    /// What a producer follows when several policies name it, over `range`: the
    /// most conservative of each knob — the highest carry ceiling, the `Land` gate
    /// relaxed when any relaxes it.
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

    /// Whether a policy that asks anything names producers that will follow it,
    /// judged by the producer's own rule ([`Self::applies_to`]). `handing_over` is
    /// every build that can hand its sessions to the release sealing this policy:
    /// the builds claimed up to the newest release that shipped before it. The
    /// policy must apply to one of them that follows a policy at all
    /// ([`FIRST_POLICY_FOLLOWING_BUILD`] on) — a range holding none (a digit
    /// dropped from a ten-digit build, or only builds of releases cut before the
    /// reader) ships a rescue no producer follows, the silent no-op the strict key
    /// read exists to prevent — and to none after the newest of them: every later
    /// release must keep what this one seals ([`PolicyFile::keeps`]), so a range
    /// reaching past the builds that exist would bind the producers of releases
    /// cut later, which never had the bug. Builds only grow and a ledger only
    /// appends, so a range that passes here passes again at every later cut that
    /// keeps it.
    ///
    /// # Errors
    /// What the range misses, in words for the cutter's refusal.
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

    /// This policy as the top-level lines a file writes for it, for a refusal that
    /// says what to write: the range, and each knob that asks anything.
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

/// One `[[ranges]]` table: a rescue for the producer builds it names
/// (`policy.applies_to_producers`, always set), the knobs they follow, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rescue {
    /// The builds and their knobs.
    pub policy: HandoffPolicy,
    /// What it routes around. The cutter requires one; a producer keeps whatever
    /// the file says, or nothing.
    pub reason: String,
}

impl Rescue {
    /// The `ranges` key's value read as a list of rescues. `strict` is the
    /// cutter's read: no key a table does not define, a reason, and a knob that
    /// asks something.
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

/// What a file's `ranges` key holds, as this build reads it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Ranges {
    /// No `ranges` key: the top-level keys are the whole policy, read as every
    /// older build reads them.
    #[default]
    Absent,
    /// The rescues, read.
    Listed(Vec<Rescue>),
    /// A list this build cannot read, and why: it follows the top-level keys
    /// instead, the hull every older build follows.
    Unreadable(String),
}

/// What one policy file says: the top-level keys every producer before
/// `[[ranges]]` reads alone, and the rescues a producer from then on reads
/// instead (the module doc's SEVERAL RESCUES).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PolicyFile {
    /// The top-level keys ([`HandoffPolicy::parse`]): the whole policy without
    /// `[[ranges]]`, the hull of the list beside one.
    pub legacy: HandoffPolicy,
    /// `[[ranges]]`, as this build reads it.
    pub ranges: Ranges,
}

impl PolicyFile {
    /// Parse a file the way a PRODUCER from `[[ranges]]` on reads one: the
    /// top-level keys by [`HandoffPolicy::parse`]'s rules (an `Err` is the whole
    /// file ignored), and the list leniently — unknown keys in a table ignored, and
    /// a list this build cannot read kept as [`Ranges::Unreadable`], so the
    /// producer follows the hull.
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

    /// Parse the checked-in file the way the RELEASE CUTTER reads it: everything
    /// [`Self::parse`] refuses, plus any key schema v1 does not define (a typo such
    /// as `cary = "repaint"` would otherwise ship as a policy no producer follows),
    /// a malformed `[[ranges]]` or `[[retired]]` table, a knob outside
    /// `[[ranges]]`, top-level keys that are not the list's hull ([`Self::hull`]),
    /// and a range both rescued and retired.
    ///
    /// # Errors
    /// Why, in words for the cutter's refusal.
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

    /// The HULL of `rescues` — what a file's top-level keys say beside them, and
    /// so all a build before `[[ranges]]` follows: the lowest `min` to the highest
    /// `max`, the highest carry ceiling, and the `Land` gate relaxed when any
    /// rescue relaxes it. `None` for no rescue.
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

    /// What a producer running `build` follows, if anything is for it: with
    /// `[[ranges]]`, every rescue whose range names `build`, combined
    /// ([`HandoffPolicy::combined`], over the range they share), and `None` when
    /// none names it; without the list, or with one this build cannot read, the
    /// top-level keys, as every older build reads them.
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

    /// The rescues the file holds, each with its range and reason: the
    /// `[[ranges]]` tables, or without them the top-level keys as one rescue when
    /// they ask anything (every policy sealed before `[[ranges]]`).
    ///
    /// # Errors
    /// A list this build cannot read, and why.
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

    /// [`HandoffPolicy::names_producers`] for every rescue the file holds, the
    /// refusal naming the rescue's builds.
    ///
    /// # Errors
    /// The first rescue whose range misses, and what it misses.
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

    /// Whether this file KEEPS `sealed`, one rescue an earlier release sealed, as a
    /// producer that reads `[[ranges]]` follows it ([`Self::for_build`]): every
    /// build `sealed` names is still named, at a carry ceiling at least as
    /// conservative, with the `Land` gate relaxed wherever `sealed` relaxed it. A
    /// wider range, another rescue covering part of it, or a more conservative knob
    /// all keep it; the hull an older build reads is at least the list (the cutter
    /// holds the top-level keys to it), so it keeps it too. A producer reads only
    /// the release it hands over to, so a rescue a later release drops stops
    /// protecting a build that skipped the rescuing one. A `sealed` that asks
    /// nothing is kept by anything.
    ///
    /// # Errors
    /// The first build it drops, and how, in words for the cutter's refusal.
    pub fn keeps(&self, sealed: &HandoffPolicy) -> Result<(), String> {
        if !sealed.asks_anything() {
            return Ok(());
        }
        let named = sealed.applies_to_producers.unwrap_or(ProducerRange {
            min_build: 0,
            max_build: u64::MAX,
        });
        // What `for_build` answers changes only where a range it reads starts or
        // ends, so the first build of each stretch between those edges stands for
        // the whole stretch.
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

/// One `[[retired]]` table: the cutter's record that the rescue an earlier
/// release sealed for exactly `producers` was dropped on purpose, why, and when,
/// so the cut may drop what [`PolicyFile::keeps`] would otherwise hold it to (the
/// cutter prints the reason in its transcript). Only the cutter reads the list;
/// a producer ignores it like any key it does not know. It is a RECORD, kept in
/// later releases: the cutter refuses an entry naming a range the file still
/// rescues, or one the previous release sealed and this file keeps, so no entry
/// can license a later drop of a rescue sealed again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Retired {
    /// The sealed rescue's range, spelled exactly.
    pub producers: ProducerRange,
    /// Why no build in that range needs the rescue any more.
    pub reason: String,
    /// When it was retired, `YYYY-MM-DD`.
    pub date: String,
}

impl Retired {
    /// The one shape a `[[retired]]` table takes, for a refusal.
    const SHAPE: &str = "`producers = [min_build, max_build]`, `reason = \"why no build in \
                         that range needs the rescue\"`, `date = \"YYYY-MM-DD\"`";

    /// The file's `[[retired]]` list, read strictly.
    ///
    /// # Errors
    /// Not TOML, or a `retired` that is not a list of tables holding exactly
    /// [`Self::SHAPE`]'s keys, with a non-empty reason and a calendar date.
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

/// Whether `text` is a `YYYY-MM-DD` date with a month from 1 to 12 and a day
/// from 1 to 31.
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

/// `text` parsed as TOML whose root is a table.
fn table_of(text: &str) -> Result<aterm_toml::Table, String> {
    match aterm_toml::from_str(text).map_err(|error| format!("not TOML: {error}"))? {
        aterm_toml::Value::Table(table) => Ok(table),
        _ => Err("not a TOML table".to_string()),
    }
}

/// The two knobs of a table — the top level, or one `[[ranges]]` table.
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

/// `key`'s `[min_build, max_build]`.
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

/// What reading a candidate bundle's policy file found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PolicyRead {
    /// The bundle carries no policy file — every release before this one.
    Absent,
    /// A schema-v1 policy file, as a producer reads it ([`PolicyFile::parse`]).
    Parsed(PolicyFile),
    /// A file that is there but cannot be followed, and why. Ignored: the producer
    /// behaves exactly as with no file.
    Ignored(String),
    /// The read never reached the file's bytes — the system refused it FOR NOW (no
    /// descriptor left, no memory, an I/O error, an interrupted call) — and why.
    ///
    /// NOT A VERDICT ON THE FILE (round seven, H1 finding 57). Filed as
    /// [`Self::Ignored`], a moment of descriptor exhaustion was cached with the
    /// candidate's verified pass as "the successor asks nothing", and for the pass's
    /// freshness window every park ran the very capture the release had sealed a
    /// policy to route around. A reader treats this as "unknown": the pre-verify
    /// refuses as a passing condition, and the next attempt reads the file again.
    Unread(String),
}

/// Read the policy of the bundle rooted at `app_root`.
///
/// ONLY EVER CALLED ON A BUNDLE THAT HAS PASSED ITS CODESIGN CHECK — see the module
/// doc for why that is the whole trust argument; `aterm-update`'s pre-verification
/// is the one caller, and it reads after the check and never on a refusal.
///
/// The file must be a regular file (a symlink is never followed) no larger than
/// [`MAX_POLICY_BYTES`], holding UTF-8 TOML.
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
    /// A fact about the FILE: not a regular file, too large, not UTF-8, or an error
    /// the same file would give again.
    File(String),
    /// A fact about this MOMENT ([`is_momentary`]): the bytes were never reached.
    Moment(String),
}

/// Whether an I/O error says the system could not serve the read just then, not
/// that the file is unreadable: out of descriptors (`EMFILE`, `ENFILE`), out of
/// memory or buffers (`ENOMEM`, `ENOBUFS`), a device error (`EIO`), an interrupted
/// or would-block call (`EINTR`, `EAGAIN`).
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

/// [`is_momentary`] for the OTHER facts a pre-verification reads off the same
/// verified bundle at the same instant (round eight of the update audit, U19: the
/// candidate's `Info.plist`, whose chunked-grant declaration is cached beside this
/// policy): one sorting of a read error into "the system refused it for now" and
/// "a fact about the file", so a moment can never be filed as a verdict on one
/// fact while the policy beside it is refused as unread.
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

/// `Ok(None)` when there is no file at all; `Err` for one that cannot be read as a
/// small regular UTF-8 file.
///
/// OPENED FIRST, JUDGED BY WHAT WAS OPENED. The path is opened without following a
/// link and WITHOUT BLOCKING, and the descriptor itself must be a regular file
/// within [`MAX_POLICY_BYTES`] before a byte is read. A check of the path followed
/// by an open leaves a window in which a same-user process can rename a FIFO onto
/// it, and a blocking `open(2)` of a FIFO waits for a writer that may never come —
/// with the apply lock held on the staged lane, and every PTY reader parked on the
/// fork lane (the same race `aterm-gui`'s seamless reader closes the same way).
/// A FIFO opened non-blocking returns at once and is refused as not a regular file.
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
        // `O_NOFOLLOW` on a symbolic link: never followed, never read.
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
    let mut bytes = Vec::new();
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

/// What a producer running `producer_build` does with what it read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Adoption {
    /// Nothing to follow: no file, or a policy that asks for nothing.
    None,
    /// A policy this producer must follow at its next park: what the file asks of
    /// its build ([`PolicyFile::for_build`]).
    Follow(HandoffPolicy),
    /// A policy for other producers; this one ignores it.
    NotForThisBuild(PolicyFile),
    /// A file this producer cannot follow, and why.
    Ignored(String),
}

impl Adoption {
    /// Decide, purely, what `read` means for a producer running `producer_build`.
    #[must_use]
    pub fn of(read: &PolicyRead, producer_build: u64) -> Self {
        match read {
            PolicyRead::Absent => Self::None,
            PolicyRead::Ignored(why) => Self::Ignored(why.clone()),
            // Never reached through `aterm-update`'s pre-verifications, which refuse
            // an unread policy as a passing condition before any caller adopts it;
            // total here, and read as the one thing it can be: nothing to follow.
            PolicyRead::Unread(why) => Self::Ignored(format!("not read just then: {why}")),
            PolicyRead::Parsed(file) => match file.for_build(producer_build) {
                None => Self::NotForThisBuild(file.clone()),
                Some(policy) if policy.asks_anything() => Self::Follow(policy),
                Some(_) => Self::None,
            },
        }
    }

    /// The policy to follow, if any.
    #[must_use]
    pub fn policy(&self) -> Option<HandoffPolicy> {
        match self {
            Self::Follow(policy) => Some(*policy),
            Self::None | Self::NotForThisBuild(_) | Self::Ignored(_) => None,
        }
    }
}

/// [`Adoption::of`], plus the ONE log line it deserves: a WARN for a file that
/// cannot be followed, an INFO for a policy that is followed or is not for this
/// build, nothing when there is nothing to say. `candidate` names the bundle for
/// the log ("staged update build 1790…").
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
mod tests {
    use super::*;

    /// A wait for something that must happen is a hang detector, never a latency budget.
    const HANG_DETECTOR: std::time::Duration = std::time::Duration::from_secs(60);

    fn policy(text: &str) -> HandoffPolicy {
        HandoffPolicy::parse(text).unwrap_or_else(|why| panic!("{text:?}: {why}"))
    }

    #[test]
    fn every_v1_key_parses_and_every_key_is_optional() {
        assert_eq!(policy("schema = 1\n"), HandoffPolicy::default());
        assert_eq!(
            policy(
                "schema = 1\napplies_to_producers = [1790000000, 1790999999]\ncarry = \
                 \"repaint\"\npark_quiet_gate_at_land = \"relaxed\"\n"
            ),
            HandoffPolicy {
                applies_to_producers: Some(ProducerRange {
                    min_build: 1_790_000_000,
                    max_build: 1_790_999_999,
                }),
                carry: Some(CarryCeiling::Repaint),
                park_quiet_gate_at_land: Some(LandQuietGate::Relaxed),
            }
        );
        for (spelling, ceiling) in [
            ("full", CarryCeiling::Full),
            ("visible", CarryCeiling::Visible),
            ("repaint", CarryCeiling::Repaint),
        ] {
            assert_eq!(
                policy(&format!("schema = 1\ncarry = \"{spelling}\"\n")).carry_ceiling(),
                ceiling
            );
        }
        assert!(
            !policy("schema = 1\npark_quiet_gate_at_land = \"strict\"\n").relaxes_land_gate(),
            "strict is the gate as it ships"
        );
    }

    /// "visible" and "repaint" carry NO scrollback — the one reading both of the
    /// producer's scrollback carries take — and the order runs from most faithful
    /// to least, so a greater ceiling asks for less (what the fork lane's check of
    /// a capture against a policy read after it compares).
    #[test]
    fn only_a_full_ceiling_carries_scrollback_and_greater_is_more_conservative() {
        assert!(CarryCeiling::Full.carries_scrollback());
        assert!(!CarryCeiling::Visible.carries_scrollback());
        assert!(!CarryCeiling::Repaint.carries_scrollback());
        assert!(CarryCeiling::Full < CarryCeiling::Visible);
        assert!(CarryCeiling::Visible < CarryCeiling::Repaint);
        assert_eq!(
            CarryCeiling::Full.max(CarryCeiling::Repaint),
            CarryCeiling::Repaint,
            "the max of two ceilings is the more conservative"
        );
    }

    /// A later release may ADD keys without a schema bump, so a producer ignores
    /// what it does not know — including the reserved `seamless`, which no build
    /// implements, and an unknown key inside a `[[ranges]]` table. The cutter,
    /// reading our own file, refuses all three.
    #[test]
    fn unknown_keys_are_ignored_by_a_producer_and_refused_by_the_cutter() {
        let text = "schema = 1\ncarry = \"visible\"\nsome_future_knob = 3\n[future]\nx = 1\n";
        assert_eq!(policy(text).carry, Some(CarryCeiling::Visible));
        let refused = PolicyFile::parse_for_cut(text).expect_err("a typo refuses the cut");
        assert!(refused.contains("unknown key"), "{refused}");
        let reserved = "schema = 1\nseamless = false\n";
        assert_eq!(policy(reserved), HandoffPolicy::default());
        let refused = PolicyFile::parse_for_cut(reserved).expect_err("reserved");
        assert!(refused.contains("`seamless` is reserved"), "{refused}");
        let future_rescue = format!("{HULL_AB}\n{RESCUE_A}future_knob = 1\n\n{RESCUE_B}");
        assert_eq!(
            file(&future_rescue)
                .for_build(150)
                .map(|p| p.carry_ceiling()),
            Some(CarryCeiling::Repaint),
            "a producer reads the rescue, its unknown key ignored"
        );
        let refused = PolicyFile::parse_for_cut(&future_rescue).expect_err("a typo in a table");
        assert!(
            refused.contains("[[ranges]] table 1: unknown key `future_knob`"),
            "{refused}"
        );
        assert_eq!(
            PolicyFile::parse_for_cut(&two_rescues())
                .expect("a ranged v1 policy passes the cutter")
                .legacy
                .carry,
            Some(CarryCeiling::Repaint)
        );
    }

    /// Every knob the cutter seals is a rescue with its own range and reason, so a
    /// later release can tell what each one is for and when to retire it: a knob
    /// outside `[[ranges]]` is refused, and so is a table with no range, no
    /// reason, or nothing asked. A producer still reads an unranged top-level knob
    /// as "every build" (the schema's reading rule), since refusing it there would
    /// only drop a rescue.
    #[test]
    fn the_cutter_refuses_a_knob_that_is_not_a_rescue_with_its_builds_and_reason() {
        for loose in [
            "schema = 1\ncarry = \"repaint\"\n",
            "schema = 1\npark_quiet_gate_at_land = \"relaxed\"\n",
            "schema = 1\napplies_to_producers = [100, 199]\ncarry = \"repaint\"\n",
        ] {
            let refused = PolicyFile::parse_for_cut(loose).expect_err(loose);
            assert!(refused.contains("a knob outside `[[ranges]]`"), "{refused}");
        }
        assert!(policy("schema = 1\ncarry = \"repaint\"\n").applies_to(7));
        for (table, why) in [
            (
                "carry = \"repaint\"\nreason = \"x\"\n",
                "no `producers = [min_build, max_build]`",
            ),
            (
                "producers = [100, 199]\ncarry = \"repaint\"\n",
                "no `reason`",
            ),
            (
                "producers = [100, 199]\ncarry = \"repaint\"\nreason = \"  \"\n",
                "no `reason`",
            ),
            (
                "producers = [100, 199]\ncarry = \"full\"\nreason = \"x\"\n",
                "asks nothing",
            ),
            (
                "producers = [199, 100]\ncarry = \"repaint\"\nreason = \"x\"\n",
                "`producers` is [199, 100], an empty range",
            ),
            (
                "producers = [100, 199]\ncarry = \"sideways\"\nreason = \"x\"\n",
                "`carry`",
            ),
        ] {
            let text = format!(
                "schema = 1\napplies_to_producers = [100, 199]\ncarry = \"repaint\"\n\n\
                 [[ranges]]\n{table}"
            );
            let refused = PolicyFile::parse_for_cut(&text).expect_err(&text);
            assert!(
                refused.contains("[[ranges]] table 1") && refused.contains(why),
                "{text}: {refused}"
            );
        }
        let refused = PolicyFile::parse_for_cut("schema = 1\nranges = []\n").expect_err("empty");
        assert!(refused.contains("lists no rescue"), "{refused}");
        for asks_nothing in [
            "schema = 1\n",
            "schema = 1\ncarry = \"full\"\npark_quiet_gate_at_land = \"strict\"\n",
        ] {
            PolicyFile::parse_for_cut(asks_nothing).expect(asks_nothing);
        }
    }

    /// Builds 100-199 panic in their capture (blank screens), builds 250-299 never
    /// open their `Land` gate (relaxed): two rescues with different knobs, under
    /// their hull.
    const RESCUE_A: &str =
        "[[ranges]]\nproducers = [100, 199]\ncarry = \"repaint\"\nreason = \"capture panics\"\n";
    const RESCUE_B: &str = "[[ranges]]\nproducers = [250, 299]\npark_quiet_gate_at_land = \
                            \"relaxed\"\nreason = \"Land gate never opens\"\n";
    const HULL_AB: &str = "schema = 1\napplies_to_producers = [100, 299]\ncarry = \
                           \"repaint\"\npark_quiet_gate_at_land = \"relaxed\"\n";

    fn two_rescues() -> String {
        format!("{HULL_AB}\n{RESCUE_A}\n{RESCUE_B}")
    }

    fn file(text: &str) -> PolicyFile {
        PolicyFile::parse(text).unwrap_or_else(|why| panic!("{text:?}: {why}"))
    }

    /// THE HULL (r8c finding 504): the lowest `min` to the highest `max`, the
    /// highest carry ceiling, the `Land` gate relaxed when any rescue relaxes it —
    /// so a build that reads only the top-level keys gets at least its own
    /// rescue. The cutter holds the top-level keys to it exactly, and says what to
    /// write when they are not.
    #[test]
    fn the_top_level_keys_beside_ranges_are_the_hull_of_the_rescues() {
        let Ranges::Listed(rescues) = file(&two_rescues()).ranges else {
            panic!("two rescues listed");
        };
        assert_eq!(
            PolicyFile::hull(&rescues),
            Some(HandoffPolicy {
                applies_to_producers: Some(ProducerRange {
                    min_build: 100,
                    max_build: 299,
                }),
                carry: Some(CarryCeiling::Repaint),
                park_quiet_gate_at_land: Some(LandQuietGate::Relaxed),
            })
        );
        assert_eq!(PolicyFile::hull(&[]), None);
        // A visible rescue and a repaint one: the hull carries the more
        // conservative; neither relaxes the gate, so the hull does not either.
        let visible = "[[ranges]]\nproducers = [300, 310]\ncarry = \"visible\"\nreason = \"x\"\n";
        let Ranges::Listed(rescues) = file(&format!("schema = 1\n{RESCUE_A}\n{visible}")).ranges
        else {
            panic!("listed");
        };
        let hull = PolicyFile::hull(&rescues).expect("a hull");
        assert_eq!(hull.carry_ceiling(), CarryCeiling::Repaint);
        assert!(!hull.relaxes_land_gate());

        PolicyFile::parse_for_cut(&two_rescues()).expect("the hull, written out");
        for not_the_hull in [
            two_rescues().replace("[100, 299]", "[100, 199]"),
            two_rescues().replace("park_quiet_gate_at_land = \"relaxed\"\n\n", "\n"),
            two_rescues().replace("carry = \"repaint\"\npark", "carry = \"visible\"\npark"),
            format!("schema = 1\n\n{RESCUE_A}\n{RESCUE_B}"),
        ] {
            let refused = PolicyFile::parse_for_cut(&not_the_hull).expect_err(&not_the_hull);
            assert!(
                refused.contains("not the hull of `[[ranges]]`")
                    && refused.contains(
                        "write `applies_to_producers = [100, 299]`, `carry = \"repaint\"`, \
                         `park_quiet_gate_at_land = \"relaxed\"`"
                    ),
                "{not_the_hull}: {refused}"
            );
        }
    }

    /// A PRODUCER THAT READS `[[ranges]]` follows the rescues naming its own build
    /// alone: a build in one range follows that rescue's knobs and no other's, a
    /// build two overlapping rescues name follows the more conservative of each
    /// knob, and a build in a gap between ranges, or outside them all, follows
    /// nothing — though the hull covers it.
    #[test]
    fn a_producer_that_reads_ranges_follows_only_the_rescues_naming_its_build() {
        let overlap = "[[ranges]]\nproducers = [180, 260]\ncarry = \"visible\"\nreason = \"x\"\n";
        let read = PolicyRead::Parsed(file(&format!("{}\n{overlap}", two_rescues())));
        let follows = |build: u64| Adoption::of(&read, build).policy();
        let at = |build: u64| {
            follows(build).map(|policy| (policy.carry_ceiling(), policy.relaxes_land_gate()))
        };
        assert_eq!(at(100), Some((CarryCeiling::Repaint, false)));
        assert_eq!(at(150), Some((CarryCeiling::Repaint, false)));
        assert_eq!(
            at(190),
            Some((CarryCeiling::Repaint, false)),
            "A and the overlap"
        );
        assert_eq!(
            at(220),
            Some((CarryCeiling::Visible, false)),
            "the overlap alone"
        );
        assert_eq!(
            at(255),
            Some((CarryCeiling::Visible, true)),
            "B and the overlap"
        );
        assert_eq!(at(280), Some((CarryCeiling::Full, true)), "B alone");
        // The resolved policy names a range holding the build: the rescues' shared
        // stretch, so a check of `applies_to` on it agrees.
        assert_eq!(
            follows(190).and_then(|policy| policy.applies_to_producers),
            Some(ProducerRange {
                min_build: 180,
                max_build: 199,
            })
        );
        for outside in [99, 300, u64::MAX] {
            assert!(
                matches!(Adoption::of(&read, outside), Adoption::NotForThisBuild(_)),
                "build {outside}"
            );
        }
        let gap = PolicyRead::Parsed(file(&two_rescues()));
        assert!(
            matches!(Adoption::of(&gap, 220), Adoption::NotForThisBuild(_)),
            "a build in the gap between the two rescues follows neither"
        );
    }

    /// AN OLDER PRODUCER SEES THE HULL. v0.97.0 through v0.100.0 read the file
    /// with [`HandoffPolicy::parse`] — this very code: their top-level read and
    /// `aterm-toml`'s parser are unchanged since v0.97.0 but for comments — and
    /// ignore `ranges` and `retired` as keys they do not know. So each of them
    /// in a rescued range carries at least what its own rescue asks, and one in a
    /// gap carries less than it needs to: a degrade, never a refusal (L1).
    #[test]
    fn a_producer_before_ranges_reads_the_hull_and_is_never_refused() {
        let text = format!(
            "{}\n[[retired]]\nproducers = [1, 2]\nreason = \"x\"\ndate = \"2026-10-03\"\n",
            two_rescues()
        );
        let hull = policy(&text);
        assert_eq!(hull, file(&text).legacy);
        assert_eq!(
            hull,
            HandoffPolicy::parse(HULL_AB).expect("the top-level keys alone"),
            "the lists change nothing an older build reads"
        );
        // How such a build adopts: its own policy, with no list.
        let older = PolicyRead::Parsed(PolicyFile {
            legacy: hull,
            ranges: Ranges::Absent,
        });
        for build in [100, 150, 220, 280, 299] {
            let followed = Adoption::of(&older, build)
                .policy()
                .unwrap_or_else(|| panic!("build {build} follows the hull"));
            assert_eq!(followed.carry_ceiling(), CarryCeiling::Repaint);
            assert!(followed.relaxes_land_gate());
            let precise = file(&text).for_build(build);
            assert!(
                precise.is_none_or(|own| own.carry_ceiling() <= followed.carry_ceiling()
                    && (!own.relaxes_land_gate() || followed.relaxes_land_gate())),
                "build {build}: the hull asks at least its own rescue"
            );
        }
        assert!(matches!(
            Adoption::of(&older, 300),
            Adoption::NotForThisBuild(_)
        ));
    }

    /// A list this build cannot read — a value it does not know, a table with no
    /// range — leaves it on the hull, as an older build: never less than what the
    /// release asked of it, and never the whole file ignored.
    #[test]
    fn a_ranges_list_this_build_cannot_read_leaves_it_on_the_hull() {
        for unreadable in [
            format!("{HULL_AB}\n[[ranges]]\nproducers = [100, 199]\ncarry = \"minimal\"\n"),
            format!("{HULL_AB}\n[[ranges]]\ncarry = \"repaint\"\n"),
            format!("{HULL_AB}ranges = 3\n"),
        ] {
            let read = file(&unreadable);
            assert!(
                matches!(&read.ranges, Ranges::Unreadable(_)),
                "{unreadable}: {read:?}"
            );
            let followed = Adoption::of(&PolicyRead::Parsed(read.clone()), 220)
                .policy()
                .unwrap_or_else(|| panic!("{unreadable}: the hull is followed"));
            assert_eq!(followed, read.legacy, "{unreadable}");
            assert!(read.rescues().is_err(), "the cutter cannot judge it either");
        }
    }

    /// `[[retired]]` is the cutter's record: it reads it strictly — a range, a
    /// reason and a date, nothing else — and a producer ignores it like any key it
    /// does not know. The one-cut `retire` key it replaced is refused with the new
    /// spelling, and a range both rescued and retired is refused.
    #[test]
    fn a_retirement_is_read_strictly_by_the_cutter_and_ignored_by_a_producer() {
        let entry = "[[retired]]\nproducers = [10, 20]\nreason = \"all past EOL\"\ndate = \
                     \"2026-10-03\"\n";
        let text = format!("schema = 1\n\n{entry}");
        assert_eq!(
            Retired::parse_for_cut(&text),
            Ok(vec![Retired {
                producers: ProducerRange {
                    min_build: 10,
                    max_build: 20,
                },
                reason: "all past EOL".to_string(),
                date: "2026-10-03".to_string(),
            }])
        );
        assert_eq!(PolicyFile::parse_for_cut(&text), Ok(PolicyFile::default()));
        assert_eq!(file(&text), PolicyFile::default());
        assert_eq!(Retired::parse_for_cut("schema = 1\n"), Ok(Vec::new()));
        for malformed in [
            "producers = [10, 20]\nreason = \"x\"\n",
            "producers = [10, 20]\ndate = \"2026-10-03\"\n",
            "producers = [10, 20]\nreason = \"  \"\ndate = \"2026-10-03\"\n",
            "producers = [20, 10]\nreason = \"x\"\ndate = \"2026-10-03\"\n",
            "producers = [10]\nreason = \"x\"\ndate = \"2026-10-03\"\n",
            "producers = [10, 20]\nreason = \"x\"\ndate = \"2026-13-03\"\n",
            "producers = [10, 20]\nreason = \"x\"\ndate = \"3 October\"\n",
            "producers = [10, 20]\nreason = \"x\"\ndate = \"2026-10-03\"\nextra = 1\n",
        ] {
            let text = format!("schema = 1\n\n[[retired]]\n{malformed}");
            let refused = PolicyFile::parse_for_cut(&text).expect_err(&text);
            assert!(
                refused.contains("[[retired]] table 1 is not")
                    && refused.contains("`date = \"YYYY-MM-DD\"`"),
                "{text}: {refused}"
            );
        }
        let refused = PolicyFile::parse_for_cut("schema = 1\nretired = 3\n").expect_err("3");
        assert!(
            refused.contains("not a list of [[retired]] tables"),
            "{refused}"
        );
        let refused = PolicyFile::parse_for_cut(
            "schema = 1\nretire = { producers = [10, 20], reason = \"x\" }\n",
        )
        .expect_err("the old spelling");
        assert!(
            refused.contains("`retire` is now a `[[retired]]` table"),
            "{refused}"
        );
        let both = format!(
            "{}\n[[retired]]\nproducers = [250, 299]\nreason = \"x\"\ndate = \"2026-10-03\"\n",
            two_rescues()
        );
        let refused = PolicyFile::parse_for_cut(&both).expect_err("rescued and retired");
        assert!(
            refused.contains("builds [250, 299] are both rescued"),
            "{refused}"
        );
    }

    /// Anything a v1 reader cannot interpret is the whole file ignored — never a
    /// guess, and never a stricter park than the build ships with.
    #[test]
    fn a_malformed_policy_is_refused_whole_and_says_why() {
        for (text, why) in [
            ("carry = \"repaint\"\n", "no `schema = 1` line"),
            ("schema = 2\ncarry = \"repaint\"\n", "schema 2"),
            ("schema = \"1\"\n", "not an integer"),
            ("schema = 1\ncarry = \"sideways\"\n", "`carry`"),
            ("schema = 1\ncarry = 3\n", "`carry`"),
            (
                "schema = 1\npark_quiet_gate_at_land = \"loose\"\n",
                "park_quiet",
            ),
            (
                "schema = 1\napplies_to_producers = [5]\n",
                "applies_to_producers",
            ),
            (
                "schema = 1\napplies_to_producers = [1, 2, 3]\n",
                "applies_to",
            ),
            ("schema = 1\napplies_to_producers = [-1, 2]\n", "applies_to"),
            (
                "schema = 1\napplies_to_producers = [\"a\", 2]\n",
                "applies_to",
            ),
            ("schema = 1\napplies_to_producers = [9, 2]\n", "empty range"),
            ("schema = 1\napplies_to_producers = 7\n", "applies_to"),
            ("schema = 1\ncarry = \"repaint\n", "not TOML"),
        ] {
            let refused = HandoffPolicy::parse(text).expect_err(text);
            assert!(refused.contains(why), "{text:?}: {refused}");
            assert_eq!(
                PolicyFile::parse(text).expect_err(text),
                refused,
                "{text:?}"
            );
        }
    }

    /// A later release keeps what an earlier one sealed: the same or a wider
    /// range, a ceiling no less conservative, the Land gate still relaxed. Each
    /// way of dropping a knob is refused by name, and a sealed policy that asks
    /// nothing binds nothing.
    #[test]
    fn a_later_policy_keeps_every_knob_an_earlier_one_sealed() {
        let sealed = policy("schema = 1\napplies_to_producers = [10, 20]\ncarry = \"visible\"\n");
        for kept in [
            "schema = 1\napplies_to_producers = [10, 20]\ncarry = \"visible\"\n",
            "schema = 1\napplies_to_producers = [5, 30]\ncarry = \"visible\"\n",
            "schema = 1\napplies_to_producers = [10, 20]\ncarry = \"repaint\"\n",
            "schema = 1\ncarry = \"visible\"\n",
        ] {
            assert_eq!(file(kept).keeps(&sealed), Ok(()), "{kept}");
        }
        for (dropped, why) in [
            ("schema = 1\n", "raises `carry`"),
            (
                "schema = 1\napplies_to_producers = [11, 20]\ncarry = \"visible\"\n",
                "no longer applies",
            ),
            (
                "schema = 1\napplies_to_producers = [10, 19]\ncarry = \"repaint\"\n",
                "no longer applies",
            ),
            (
                "schema = 1\napplies_to_producers = [10, 20]\ncarry = \"full\"\n",
                "raises `carry`",
            ),
        ] {
            let refused = file(dropped).keeps(&sealed).expect_err(dropped);
            assert!(refused.contains(why), "{dropped}: {refused}");
        }
        let everyone = policy("schema = 1\ncarry = \"repaint\"\n");
        assert!(
            file("schema = 1\napplies_to_producers = [0, 99]\ncarry = \"repaint\"\n")
                .keeps(&everyone)
                .is_err()
        );
        let relaxed = policy("schema = 1\npark_quiet_gate_at_land = \"relaxed\"\n");
        assert!(
            file("schema = 1\npark_quiet_gate_at_land = \"strict\"\n")
                .keeps(&relaxed)
                .is_err()
        );
        assert_eq!(
            file("schema = 1\npark_quiet_gate_at_land = \"relaxed\"\n").keeps(&relaxed),
            Ok(())
        );
        for asks_nothing in [
            "schema = 1\n",
            "schema = 1\napplies_to_producers = [1, 2]\ncarry = \"full\"\n",
        ] {
            assert_eq!(
                file("schema = 1\n").keeps(&policy(asks_nothing)),
                Ok(()),
                "{asks_nothing}"
            );
        }
    }

    /// A sealed rescue is kept by the RANGES a producer reads, build by build:
    /// split across two tables, inside a wider one, or under an overlap that
    /// asks more. A gap opened inside it, a weaker knob on part of it, or the
    /// relaxed gate dropped for one stretch is refused, naming the first build
    /// it drops.
    #[test]
    fn a_sealed_rescue_is_kept_build_by_build_through_any_ranges_that_cover_it() {
        let sealed = policy(
            "schema = 1\napplies_to_producers = [100, 199]\ncarry = \"visible\"\n\
             park_quiet_gate_at_land = \"relaxed\"\n",
        );
        let ranged = |tables: &[(u64, u64, &str)]| {
            let mut text = "schema = 1\n".to_string();
            for (min, max, knobs) in tables {
                text.push_str(&format!(
                    "\n[[ranges]]\nproducers = [{min}, {max}]\n{knobs}reason = \"r\"\n"
                ));
            }
            file(&text)
        };
        const KEPT: &str = "carry = \"visible\"\npark_quiet_gate_at_land = \"relaxed\"\n";
        const MORE: &str = "carry = \"repaint\"\npark_quiet_gate_at_land = \"relaxed\"\n";
        for (label, tables) in [
            ("the same", vec![(100, 199, KEPT)]),
            ("wider", vec![(50, 250, KEPT)]),
            ("split", vec![(100, 149, KEPT), (150, 199, MORE)]),
            (
                "an overlap asking more",
                vec![(100, 199, KEPT), (120, 300, "carry = \"repaint\"\n")],
            ),
            (
                "knobs from two tables",
                vec![
                    (100, 199, "carry = \"visible\"\n"),
                    (90, 210, "park_quiet_gate_at_land = \"relaxed\"\n"),
                ],
            ),
        ] {
            assert_eq!(ranged(&tables).keeps(&sealed), Ok(()), "{label}");
        }
        for (label, tables, why) in [
            (
                "a gap",
                vec![(100, 149, KEPT), (151, 199, KEPT)],
                "no longer applies to build 150",
            ),
            (
                "short at the top",
                vec![(100, 198, KEPT)],
                "no longer applies to build 199",
            ),
            (
                "a weaker stretch",
                vec![
                    (100, 149, KEPT),
                    (
                        150,
                        199,
                        "carry = \"full\"\npark_quiet_gate_at_land = \"relaxed\"\n",
                    ),
                ],
                "raises `carry` for build 150",
            ),
            (
                "the gate dropped for a stretch",
                vec![(100, 159, KEPT), (160, 199, "carry = \"visible\"\n")],
                "drops `park_quiet_gate_at_land = \"relaxed\"` for build 160",
            ),
        ] {
            let refused = ranged(&tables).keeps(&sealed).expect_err(label);
            assert!(refused.contains(why), "{label}: {refused}");
        }
    }

    #[test]
    fn the_range_is_inclusive_and_absent_means_every_producer() {
        let ranged = policy("schema = 1\napplies_to_producers = [100, 200]\ncarry = \"visible\"\n");
        for (build, applies) in [
            (0, false),
            (99, false),
            (100, true),
            (150, true),
            (200, true),
            (201, false),
            (u64::MAX, false),
        ] {
            assert_eq!(ranged.applies_to(build), applies, "build {build}");
        }
        let single = policy("schema = 1\napplies_to_producers = [7, 7]\n");
        assert!(single.applies_to(7) && !single.applies_to(6) && !single.applies_to(8));
        let everyone = policy("schema = 1\ncarry = \"visible\"\n");
        assert!(everyone.applies_to(0) && everyone.applies_to(u64::MAX));
    }

    #[test]
    fn a_producer_follows_only_a_policy_that_asks_something_of_its_own_build() {
        let parsed = |text: &str| PolicyRead::Parsed(file(text));
        let repaint = parsed("schema = 1\napplies_to_producers = [10, 20]\ncarry = \"repaint\"\n");
        assert_eq!(
            Adoption::of(&repaint, 15)
                .policy()
                .map(|p| p.carry_ceiling()),
            Some(CarryCeiling::Repaint)
        );
        assert!(matches!(
            Adoption::of(&repaint, 21),
            Adoption::NotForThisBuild(_)
        ));
        assert_eq!(Adoption::of(&repaint, 9).policy(), None);
        // An empty policy — the checked-in default — and a policy that asks for
        // the build's own behaviour are nothing to follow.
        for nothing in [
            "schema = 1\n",
            "schema = 1\ncarry = \"full\"\npark_quiet_gate_at_land = \"strict\"\n",
        ] {
            assert_eq!(
                Adoption::of(&parsed(nothing), 15),
                Adoption::None,
                "{nothing}"
            );
        }
        assert_eq!(Adoption::of(&PolicyRead::Absent, 15), Adoption::None);
        assert_eq!(
            Adoption::of(&PolicyRead::Ignored("bad".to_string()), 15),
            Adoption::Ignored("bad".to_string())
        );
        let relaxed = parsed("schema = 1\npark_quiet_gate_at_land = \"relaxed\"\n");
        assert!(
            Adoption::of(&relaxed, 15)
                .policy()
                .is_some_and(|p| p.relaxes_land_gate() && p.carry_ceiling() == CarryCeiling::Full)
        );
    }

    /// A scratch `.app` root with the policy file written as `write` says.
    fn scratch_bundle(label: &str, write: impl FnOnce(&Path)) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "aterm-handoff-policy-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let resources = root.join("Contents/Resources");
        std::fs::create_dir_all(&resources).expect("scratch bundle");
        write(&root.join(BUNDLE_PATH));
        root
    }

    #[test]
    fn a_bundle_policy_is_read_absent_or_ignored_never_followed_through_a_link() {
        let absent = scratch_bundle("absent", |_| {});
        assert_eq!(read_from_bundle(&absent), PolicyRead::Absent);

        let parsed = scratch_bundle("parsed", |path| {
            std::fs::write(path, "schema = 1\ncarry = \"visible\"\n").expect("write")
        });
        assert_eq!(
            read_from_bundle(&parsed),
            PolicyRead::Parsed(PolicyFile {
                legacy: HandoffPolicy {
                    carry: Some(CarryCeiling::Visible),
                    ..HandoffPolicy::default()
                },
                ranges: Ranges::Absent,
            })
        );

        let malformed = scratch_bundle("malformed", |path| {
            std::fs::write(path, "schema = 1\ncarry = \"sideways\"\n").expect("write")
        });
        assert!(
            matches!(read_from_bundle(&malformed), PolicyRead::Ignored(why) if why.contains("carry"))
        );

        let huge = scratch_bundle("huge", |path| {
            let mut text = "schema = 1\n".to_string();
            text.push_str(&"# padding\n".repeat(2048));
            std::fs::write(path, text).expect("write")
        });
        assert!(
            matches!(read_from_bundle(&huge), PolicyRead::Ignored(why) if why.contains("bound"))
        );

        let binary = scratch_bundle("binary", |path| {
            std::fs::write(path, [0xff, 0xfe, 0x00]).expect("write")
        });
        assert!(
            matches!(read_from_bundle(&binary), PolicyRead::Ignored(why) if why.contains("UTF-8"))
        );

        let directory = scratch_bundle("directory", |path| {
            std::fs::create_dir_all(path).expect("mkdir")
        });
        assert!(
            matches!(read_from_bundle(&directory), PolicyRead::Ignored(why) if why.contains("regular"))
        );

        #[cfg(unix)]
        {
            let elsewhere = std::env::temp_dir().join(format!(
                "aterm-handoff-policy-target-{}.toml",
                std::process::id()
            ));
            std::fs::write(&elsewhere, "schema = 1\ncarry = \"repaint\"\n").expect("write");
            let linked = scratch_bundle("linked", |path| {
                std::os::unix::fs::symlink(&elsewhere, path).expect("symlink")
            });
            assert!(
                matches!(read_from_bundle(&linked), PolicyRead::Ignored(why) if why.contains("regular")),
                "a link out of the bundle is never followed"
            );
            let _ = std::fs::remove_file(&elsewhere);
            let _ = std::fs::remove_dir_all(&linked);
        }
        for root in [absent, parsed, malformed, huge, binary, directory] {
            let _ = std::fs::remove_dir_all(root);
        }
    }

    /// A READ THE SYSTEM REFUSED FOR NOW IS UNREAD, NOT IGNORED (round seven, H1
    /// finding 57). Out of descriptors (a launchd-started window's soft limit of 256
    /// on a large desk), out of memory, an I/O error: the file's bytes were never
    /// reached, so nothing is known about what it asks. Filed as `Ignored` — the
    /// verdict for a malformed file — it was cached as "the successor asks nothing"
    /// and every park for ten minutes ran the capture the policy was sealed to avoid.
    /// An error the same file would give again stays a verdict on the file.
    #[cfg(unix)]
    #[test]
    fn a_read_the_system_refused_for_now_is_unread_and_a_file_error_is_ignored() {
        for errno in [
            libc::EMFILE,
            libc::ENFILE,
            libc::ENOMEM,
            libc::ENOBUFS,
            libc::EIO,
            libc::EINTR,
            libc::EAGAIN,
        ] {
            let error = std::io::Error::from_raw_os_error(errno);
            assert!(
                matches!(unreadable(&error), ReadFailure::Moment(why) if why.contains("unreadable")),
                "errno {errno} ({error}) is a moment"
            );
            // The one sorting the Info.plist read beside it shares (U19).
            assert!(is_momentary_read_error(&error), "errno {errno}");
        }
        for errno in [libc::EACCES, libc::EPERM, libc::EISDIR, libc::ENOTDIR] {
            let error = std::io::Error::from_raw_os_error(errno);
            assert!(
                matches!(unreadable(&error), ReadFailure::File(_)),
                "errno {errno} ({error}) is a fact about the file"
            );
            assert!(!is_momentary_read_error(&error), "errno {errno}");
        }
        // And what each becomes, read off a bundle.
        let at = std::path::Path::new("/x/Contents/Resources/aterm-handoff-policy.toml");
        let unread = judge(
            at,
            Err(unreadable(&std::io::Error::from_raw_os_error(libc::EMFILE))),
        );
        assert!(matches!(
            judge(
                at,
                Err(unreadable(&std::io::Error::from_raw_os_error(libc::EACCES)))
            ),
            PolicyRead::Ignored(_)
        ));
        assert!(matches!(&unread, PolicyRead::Unread(why) if why.contains("Too many open files")));
        // Adoption stays total: nothing to follow, never a policy made up.
        assert_eq!(Adoption::of(&unread, 1).policy(), None);
    }

    /// A FIFO WHERE THE POLICY SHOULD BE IS REFUSED AT ONCE, NEVER WAITED ON. The
    /// reader runs with the apply lock held (the staged lane) or with every PTY
    /// reader parked (the fork lane), and a blocking `open(2)` of a FIFO waits for
    /// a writer that never comes. The file is opened first and judged by what was
    /// opened, so this is exactly the open a FIFO renamed onto the path between a
    /// check and an open would meet. RED with a blocking open: the reader parks in
    /// `open` and the hang detector's `recv_timeout` below gives up.
    #[cfg(unix)]
    #[test]
    fn a_fifo_in_place_of_the_policy_is_refused_without_waiting_for_a_writer() {
        let fifo = scratch_bundle("fifo", |path| {
            let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
                .expect("no NUL in a temp path");
            // SAFETY: `name` is a valid NUL-terminated path for the call.
            let made = unsafe { libc::mkfifo(name.as_ptr(), 0o600) };
            assert_eq!(made, 0, "mkfifo: {}", std::io::Error::last_os_error());
        });
        let (tx, rx) = std::sync::mpsc::channel();
        let root = fifo.clone();
        std::thread::spawn(move || {
            let _ = tx.send(read_from_bundle(&root));
        });
        let read = rx
            .recv_timeout(HANG_DETECTOR)
            .expect("the reader must refuse a FIFO at once, not wait for a writer");
        assert!(
            matches!(&read, PolicyRead::Ignored(why) if why.contains("regular")),
            "{read:?}"
        );
        let _ = std::fs::remove_dir_all(fifo);
    }
}
