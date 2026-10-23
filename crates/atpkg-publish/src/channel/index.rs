// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `atpkg-publish index`: index N+1 = the verified head N + one [`Delta`], signed by this machine, self-verified, and (with `--upload`) published after every pack it pins is public.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use atpkg::net::MANIFEST_CAP;
use atpkg::{Channel, Program};

use super::mirror::{self, MirrorOptions};
use super::{
    Log, SERVE_PROBES, Scratch, download_retried, flag_value, horizon, host_error, probe_served,
};
use crate::api::{Baseline, Build, Delta, IndexRequest, Indexer, Published, TripleDrop, Yank};
use crate::artifact::{pkgdoc, upload};
use crate::ctx::Ctx;
use crate::error::{Error, Result};
use crate::host::{HostError, Probe, Replace};
use crate::identity::signer_for;
use crate::programs::{self, POLICY};
use crate::quad::{self, Freshness, QUAD_FILES, VerifiedQuad, index_tag};
use crate::triple::{Os, Triple};

/// The keys of a `[[channels]]` table this writer renders.
pub const CHANNEL_KEYS: &[&str] = &[
    "name",
    "channel_build",
    "min_build",
    "min_build_by_program",
    "yanked",
    "pin",
    "pin_by_target",
];

/// The keys of a `[programs.<name>]` block this writer renders (and `requires`, read so that a program naming it is refused by name).
pub const PROGRAM_KEYS: &[&str] = &[
    "repo",
    "policy",
    "coherence_group",
    "system",
    "unavailable_hint",
    "requires",
];

/// A horizon closer than this is said: the lanes and every client stop at it.
pub const HORIZON_WARN_SECS: i64 = 30 * 86_400;

/// The channel indexer.
#[derive(Clone, Copy, Debug, Default)]
pub struct Index;

impl Indexer for Index {
    fn publish(&self, ctx: &Ctx, request: &IndexRequest) -> Result<Published> {
        publish_with(ctx, request, &mut Log::echo("atpkg-publish index")).map(|o| o.published)
    }
}

/// What a publish is built over: the head's typed state (or an empty channel for a first publish).
#[derive(Clone, Debug)]
pub struct Base {
    /// The head's number; the new index is this + 1.
    pub index_build: u64,
    /// The roster generation that authorized the head (0 for a first publish).
    pub roster_seq: u64,
    /// The head's horizon (`None` for a first publish).
    pub valid_until: Option<String>,
    /// The head's one channel.
    pub channel: Channel,
    /// The head's program blocks.
    pub programs: BTreeMap<String, Program>,
}

impl Base {
    /// The verified head `quad`, held to what this writer can carry: exactly one `[[channels]]` block, carrying only [`CHANNEL_KEYS`], and program blocks carrying only [`PROGRAM_KEYS`].
    /// # Errors
    /// A refusal naming the block count or the keys it cannot carry.
    pub fn of_head(quad: &VerifiedQuad) -> Result<Self> {
        let raw = std::fs::read_to_string(quad.file(QUAD_FILES[0]))
            .map_err(|e| Error::io("read the head's index.toml", &e))?;
        let value: aterm_toml::Value = raw.parse().map_err(|e| {
            Error::refused(format!(
                "the head's index.toml verified but does not parse: {e}"
            ))
        })?;
        let channels = value
            .get("channels")
            .and_then(aterm_toml::Value::as_array)
            .map_or(0, Vec::len);
        if channels != 1 || quad.index.channels.len() != 1 {
            return Err(Error::refused(format!(
                "the head atpkg-index-{} carries {channels} [[channels]] blocks — this writer \
                 renders exactly one, and dropping or merging channels is not a publish",
                quad.index.index_build
            )));
        }
        let mut unknown: Vec<String> = value
            .get("channels")
            .and_then(|c| c.get(0))
            .and_then(aterm_toml::Value::as_table)
            .map(|t| {
                t.keys()
                    .filter(|k| !CHANNEL_KEYS.contains(&k.as_str()))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        if let Some(programs) = value.get("programs").and_then(aterm_toml::Value::as_table) {
            for (name, block) in programs {
                if let Some(t) = block.as_table() {
                    unknown.extend(
                        t.keys()
                            .filter(|k| !PROGRAM_KEYS.contains(&k.as_str()))
                            .map(|k| format!("programs.{name}.{k}")),
                    );
                }
            }
        }
        if !unknown.is_empty() {
            return Err(Error::refused(format!(
                "the head carries key(s) this writer cannot carry forward: {} — refusing \
                 rather than dropping them (teach the writer the key first)",
                unknown.join(", ")
            )));
        }
        Ok(Self {
            index_build: quad.index.index_build,
            roster_seq: quad.index.roster_seq(),
            valid_until: Some(quad.index.valid_until.clone()),
            channel: quad.index.channels[0].clone(),
            programs: quad.index.programs.clone(),
        })
    }

    /// A channel's genuine first publish: nothing to carry, no floors.
    /// # Errors
    /// A usage error for index 0 or an empty channel name.
    pub fn first(channel: &str, index_build: u64) -> Result<Self> {
        if index_build == 0 || channel.is_empty() {
            return Err(Error::usage(
                "a first publish names its channel and a positive index number",
            ));
        }
        Ok(Self {
            index_build: index_build - 1,
            roster_seq: 0,
            valid_until: None,
            channel: Channel {
                name: channel.to_string(),
                channel_build: 0,
                min_build: 0,
                min_build_by_program: BTreeMap::new(),
                yanked: Vec::new(),
                pin: BTreeMap::new(),
                pin_by_target: BTreeMap::new(),
            },
            programs: BTreeMap::new(),
        })
    }
}

/// One moved pin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Moved {
    /// The program.
    pub program: String,
    /// The head's build.
    pub from: Build,
    /// The new build.
    pub to: Build,
}

/// The index this publish signs, decided.
#[derive(Clone, Debug)]
pub struct Plan {
    /// `index_build` (and `channel_build`).
    pub index_build: u64,
    /// The freshness horizon.
    pub valid_until: String,
    /// The signing machine.
    pub machine_id: String,
    /// The roster generation it signs under.
    pub roster_seq: u64,
    /// The channel, every field decided.
    pub channel: Channel,
    /// Every listed program, in render order, with its block.
    pub programs: Vec<(String, Program)>,
    /// Pins moved up from the head's.
    pub moved: Vec<Moved>,
    /// Pins the head did not carry.
    pub added: Vec<(String, Build)>,
    /// Pins carried unchanged.
    pub carried: Vec<(String, Build)>,
    /// Programs retired.
    pub retired: Vec<String>,
}

/// `<name>@<build>` / `<name>@<version>`, as the client reads a `yanked` entry.
#[must_use]
pub fn yank_entry(y: &Yank) -> String {
    match y {
        Yank::Build(p, b) => format!("{p}@{b}"),
        Yank::Version(p, v) => format!("{p}@{v}"),
    }
}

fn yank_program(y: &Yank) -> &str {
    match y {
        Yank::Build(p, _) | Yank::Version(p, _) => p,
    }
}

/// A `yanked` entry: `<name>@<digits>` (the client's `is_yanked`) or `<name>@<version>` (a vendor-direct program's version latch: `x.y.z` or `x.y.z-R<n>[.<s>]`, in the one spelling `atpkg::vendor_direct::Version::parse` reads).
/// # Errors
/// A usage error naming the accepted spellings.
pub fn parse_yank(s: &str, accept_eq: bool) -> Result<Yank> {
    let bad = || {
        Error::usage(format!(
            "yank entry `{s}` is not <name>@<build> or <name>@<version> (x.y.z or \
             x.y.z-R<n>[.<s>]; --yank also takes <name>=<build>)"
        ))
    };
    let (name, spell) = match s.rsplit_once('@') {
        Some(split) => split,
        None if accept_eq => s.split_once('=').ok_or_else(bad)?,
        None => return Err(bad()),
    };
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(bad());
    }
    if !spell.is_empty() && spell.chars().all(|c| c.is_ascii_digit()) {
        let b = spell.parse().map_err(|_| bad())?;
        return Ok(Yank::Build(name.to_string(), b));
    }
    if atpkg::vendor_direct::Version::parse(spell).is_some() {
        return Ok(Yank::Version(name.to_string(), spell.to_string()));
    }
    Err(bad())
}

/// Why a new yank's version names nothing its program's client can hold, if it does: a
/// version (or a vendor build id) binds only a vendor-direct program, in that program's
/// own grammar (`VendorSpec::revision_versions`), which is all `Policy::is_yanked` matches.
fn version_mismatch(y: &Yank) -> Option<String> {
    use atpkg::vendor_direct::{Version, is_vendor_build, spec};
    let (p, version) = match y {
        Yank::Version(p, v) => (p, Version::parse(v)),
        Yank::Build(p, b) if is_vendor_build(*b) && spec(p).is_some() => {
            (p, Version::from_build_id(*b))
        }
        Yank::Build(..) => return None,
    };
    let Some(spec) = spec(p) else {
        return Some(format!(
            "{p} is not vendor-direct, and its clients match a yank by build number only"
        ));
    };
    if version.is_some_and(|v| v.has_revision() == spec.revision_versions) {
        return None;
    }
    Some(format!(
        "{p}'s versions are spelt {}",
        if spec.revision_versions {
            "x.y.z-R<n>[.<s>]"
        } else {
            "x.y.z"
        }
    ))
}

/// The refusal of any pin of vendor-direct `p`.
fn vendor_pin(p: &str) -> Error {
    Error::refused(format!(
        "{p} is vendor-direct (atpkg::vendor_direct): its client follows the vendor, and no \
         index pins it — an index that still does drops it with --retire {p}"
    ))
}

/// Admit `--pin <p>=<b>` of an ALab-built agent program `def` (its
/// [`programs::alab_package`] row): `b` is inside the client's ALab store window, and a pin
/// that switches the block from another repo (the live index's legacy row names `aterm`)
/// names a NEW build above the head's pin, which is that other repo's build.
fn admit_alab_pin(
    def: &programs::ProgramDef,
    b: Build,
    head_pin: Option<Build>,
    head_block: Option<&Program>,
) -> Result<()> {
    let p = def.name;
    crate::artifact::package::check_build(p, b)
        .map_err(|e| Error::refused(format!("--pin {p}={b}: {e}")))?;
    if let Some(old) = head_pin
        && b <= old
        && head_block.is_none_or(|blk| blk.repo != def.repo)
    {
        return Err(Error::refused(format!(
            "REFUSING --pin {p}={b}: it switches [programs.{p}] from repo {:?} to {:?}, and \
             the head's pin {old} is that other repo's build — the switch names a NEW ALab \
             build above it",
            head_block.map_or("<none>", |blk| blk.repo.as_str()),
            def.repo
        )));
    }
    Ok(())
}

/// The ALab pins `plan` moves or adds: each is signed only over a pack that verifies.
fn alab_pins(plan: &Plan) -> Vec<(&'static programs::ProgramDef, Build)> {
    plan.moved
        .iter()
        .map(|m| (m.program.as_str(), m.to))
        .chain(plan.added.iter().map(|(p, b)| (p.as_str(), *b)))
        .filter_map(|(p, b)| programs::alab_package(p).map(|def| (def, b)))
        .collect()
}

/// The block a table row renders.
fn block_of(def: &programs::ProgramDef) -> Program {
    Program {
        repo: def.repo.to_string(),
        policy: POLICY.to_string(),
        coherence_group: def.group.map(str::to_string),
        system: def.system.map(str::to_string),
        unavailable_hint: def.unavailable_hint.map(str::to_string),
        requires: Vec::new(),
    }
}

fn describe(p: &Program) -> String {
    format!(
        "repo={:?} policy={:?} group={:?} system={:?} unavailable_hint={:?}",
        p.repo, p.policy, p.coherence_group, p.system, p.unavailable_hint
    )
}

/// The index `delta` makes of `base`, signed by `machine_id` under `roster_seq`, valid until `valid_until` (already held to the clock).
/// # Errors
/// A refusal for a downgrade, an unknown or vendor-direct program, an overlay naming an unpinned program, a yank of an unpinned program that is not vendor-direct, a lift of an absent yank, a lowered floor not asked for, or a roster older than the head's.
pub fn plan(
    base: &Base,
    delta: &Delta,
    machine_id: &str,
    roster_seq: u64,
    valid_until: &str,
    log: &mut Log,
) -> Result<Plan> {
    if roster_seq < base.roster_seq {
        return Err(Error::refused(format!(
            "REFUSING roster_seq {roster_seq} < the head's {} — the roster this machine would \
             ship is OLDER than the generation already published: clients admit only the \
             newest generation on offer, so this index would reach nobody, and a fresh \
             install would admit a generation in which anyone revoked since is live again. \
             Install the current pair (it rides every atpkg-index release)",
            base.roster_seq
        )));
    }
    let head = &base.channel;

    let mut retired = Vec::new();
    for p in &delta.retire {
        if !head.pin.contains_key(p) {
            return Err(Error::usage(format!(
                "--retire {p}: the head does not pin {p} — nothing to retire"
            )));
        }
        if delta.pins.contains_key(p) {
            return Err(Error::usage(format!("{p} is both moved and retired")));
        }
        retired.push(p.clone());
    }

    let mut pin: BTreeMap<String, Build> = head
        .pin
        .iter()
        .filter(|(p, _)| !delta.retire.contains(*p))
        .map(|(p, b)| (p.clone(), *b))
        .collect();
    let mut moved = Vec::new();
    let mut added = Vec::new();
    let mut switched: BTreeSet<&str> = BTreeSet::new();
    for (p, &b) in &delta.pins {
        if atpkg::vendor_direct::is_vendor(p) {
            let Some(def) = programs::alab_package(p) else {
                return Err(vendor_pin(p));
            };
            admit_alab_pin(def, b, head.pin.get(p).copied(), base.programs.get(p))?;
            switched.insert(def.name);
        }
        if programs::get(p).is_none() && !base.programs.contains_key(p) {
            return Err(Error::refused(format!(
                "{p} is not a program this build knows (programs::PROGRAMS) — a new program \
                 is a reviewed row of the table first"
            )));
        }
        match head.pin.get(p) {
            Some(&old) if b < old => {
                return Err(Error::refused(format!(
                    "REFUSING pin downgrade for {p}: head build {old} > requested {b} — a \
                     rollback is a yank or a min_build, never a lowered pin"
                )));
            }
            Some(&old) if b > old => moved.push(Moved {
                program: p.clone(),
                from: old,
                to: b,
            }),
            Some(_) => {}
            None => added.push((p.clone(), b)),
        }
        pin.insert(p.clone(), b);
    }
    let carried: Vec<(String, Build)> = pin
        .iter()
        .filter(|(p, b)| head.pin.get(*p) == Some(b))
        .map(|(p, b)| (p.clone(), *b))
        .collect();

    let mut overlays: BTreeMap<String, BTreeMap<String, Build>> = BTreeMap::new();
    let mut dropped = Vec::new();
    for (t, cells) in &head.pin_by_target {
        for (p, &b) in cells {
            if !head.pin.contains_key(p) {
                return Err(Error::refused(format!(
                    "the head pins {p} on {t} only through pin_by_target — this writer renders \
                     overlays only for programs `pin` pins; refusing rather than dropping it"
                )));
            }
            let whole = delta
                .retire_target
                .iter()
                .any(|(rt, rp)| rt.as_str() == t && rp.is_none());
            let cell = delta
                .retire_target
                .iter()
                .any(|(rt, rp)| rt.as_str() == t && rp.as_deref() == Some(p.as_str()));
            if whole || cell {
                dropped.push(format!("{t}:{p}={b}"));
            } else if delta.retire.contains(p) {
                dropped.push(format!("{t}:{p}={b} (retired)"));
            } else {
                overlays.entry(t.clone()).or_default().insert(p.clone(), b);
            }
        }
    }
    for (t, p) in &delta.retire_target {
        let present = head
            .pin_by_target
            .get(t.as_str())
            .is_some_and(|cells| p.as_ref().is_none_or(|p| cells.contains_key(p.as_str())));
        if !present {
            return Err(Error::usage(format!(
                "--retire-target {t}{}: the head carries no such overlay — nothing to retire",
                p.as_deref().map(|p| format!(":{p}")).unwrap_or_default()
            )));
        }
    }
    let carried_cells: Vec<String> = overlays
        .iter()
        .flat_map(|(t, cells)| {
            cells
                .iter()
                .filter(|(p, _)| {
                    Triple::parse(t)
                        .ok()
                        .and_then(|tt| delta.target_pins.get(&tt))
                        .is_none_or(|new| !new.contains_key(*p))
                })
                .map(move |(p, b)| format!("{t}:{p}={b}"))
        })
        .collect();
    for (t, cells) in &delta.target_pins {
        let mut said = Vec::new();
        for (p, &b) in cells {
            if programs::alab_package(p).is_some() {
                return Err(Error::refused(format!(
                    "--target-pin {t}:{p}={b}: {p} is pinned for the targets ALab builds it \
                     for by `--pin {p}=<b>` alone — an overlay never pins it (everywhere else \
                     its client follows the vendor)"
                )));
            }
            if atpkg::vendor_direct::is_vendor(p) {
                return Err(vendor_pin(p));
            }
            if !pin.contains_key(p) {
                return Err(Error::refused(format!(
                    "--target-pin {t}:{p}={b} names {p}, which this index does not pin — an \
                     overlay rides `pin`, it never lists a program alone"
                )));
            }
            if delta
                .retire_target
                .iter()
                .any(|(rt, rp)| rt == t && rp.as_ref().is_none_or(|rp| rp == p))
            {
                return Err(Error::usage(format!("{t}:{p} is both pinned and retired")));
            }
            if let Some(&old) = head
                .pin_by_target
                .get(t.as_str())
                .and_then(|c| c.get(p.as_str()))
                && b < old
            {
                return Err(Error::refused(format!(
                    "REFUSING per-target pin downgrade for {p} on {t}: head {old} > requested \
                     {b} — a rollback is a yank or a min_build, never a lowered pin"
                )));
            }
            overlays
                .entry(t.as_str().to_string())
                .or_default()
                .insert(p.clone(), b);
            said.push(format!("{p} = {b}"));
        }
        log.say(format!("per-target pins for {t}: {}", said.join(", ")));
    }
    overlays.retain(|_, cells| !cells.is_empty());
    if !carried_cells.is_empty() {
        log.say(format!(
            "per-target pins carried from the head: {}",
            carried_cells.join(" ")
        ));
    }
    if !dropped.is_empty() {
        log.note(format!(
            "per-target pins DROPPED, not carried forward: {}",
            dropped.join(" ")
        ));
    }

    let mut yanked: Vec<Yank> = Vec::new();
    for entry in &head.yanked {
        let y = parse_yank(entry, false).map_err(|_| {
            Error::refused(format!(
                "the head's yanked entry `{entry}` is not <name>@<build> or <name>@<version> — \
                 this writer cannot carry it"
            ))
        })?;
        if !yanked.contains(&y) {
            yanked.push(y);
        }
    }
    for u in &delta.unyank {
        let Some(at) = yanked.iter().position(|y| y == u) else {
            return Err(Error::refused(format!(
                "--unyank {}: the head does not yank it — nothing to lift",
                yank_entry(u)
            )));
        };
        yanked.remove(at);
        log.note(format!("--unyank lifts {}", yank_entry(u)));
    }
    let binds = |y: &Yank| {
        let p = yank_program(y);
        pin.contains_key(p) || atpkg::vendor_direct::is_vendor(p)
    };
    for y in &delta.yank {
        if !binds(y) {
            return Err(Error::refused(format!(
                "--yank {} names {}, which this index does not pin — a revocation of a program \
                 the index does not carry revokes nothing",
                yank_entry(y),
                yank_program(y)
            )));
        }
        if let Some(why) = version_mismatch(y) {
            return Err(Error::refused(format!(
                "--yank {} matches no version any client holds: {why}",
                yank_entry(y)
            )));
        }
        if !yanked.contains(y) {
            yanked.push(y.clone());
        }
    }
    yanked.retain(|y| {
        let keep = binds(y);
        if !keep {
            log.note(format!(
                "the head's yank {} goes with {}, which this index no longer pins",
                yank_entry(y),
                yank_program(y)
            ));
        }
        keep
    });
    for y in &yanked {
        if let Yank::Build(p, b) = y
            && (pin.get(p) == Some(b) || overlays.values().any(|c| c.get(p) == Some(b)))
        {
            log.warn(format!(
                "yanked {} names the build this index PINS for {p}: every client TOMBSTONES \
                 {p} (removes it, installs nothing) until an index re-pins it. For a bad \
                 build, move the {p} pin to the fixed pack in THIS publish so the yank and \
                 the re-pin land together",
                yank_entry(y)
            ));
        }
    }
    if !yanked.is_empty() {
        log.say(format!(
            "yanked: {}",
            yanked.iter().map(yank_entry).collect::<Vec<_>>().join(" ")
        ));
    }

    let min_build = match delta.min_build {
        Some(m) if m < head.min_build && !delta.lower_min_build => {
            return Err(Error::refused(format!(
                "REFUSING min_build {m} < the head's {} — lowering the channel floor re-admits \
                 builds it revoked; say --lower-min-build on purpose",
                head.min_build
            )));
        }
        Some(m) => m,
        None => head.min_build,
    };
    let mut min_build_by_program = head.min_build_by_program.clone();
    min_build_by_program.retain(|p, _| pin.contains_key(p));

    let mut programs_out = Vec::new();
    let carried_alab = |name: &str| {
        programs::alab_package(name).is_some_and(|def| {
            !switched.contains(name) && base.programs.get(name).is_none_or(|b| b.repo != def.repo)
        })
    };
    let mut listed: BTreeSet<&str> = pin.keys().map(String::as_str).collect();
    for def in programs::PROGRAMS {
        if !carried_alab(def.name) && listed.remove(def.name) {
            programs_out.push((def.name.to_string(), block_of(def)));
        }
    }
    for p in listed {
        let block = base.programs.get(p).cloned().ok_or_else(|| {
            Error::refused(if carried_alab(p) {
                format!(
                    "the head pins {p} with no [programs.{p}] block — refusing to render \
                     ALab's row over a pin nobody named: `--pin {p}=<b>` switches it"
                )
            } else {
                format!(
                    "{p} is pinned but has no [programs.{p}] block in the head and no row in \
                     programs::PROGRAMS"
                )
            })
        })?;
        log.note(if carried_alab(p) {
            format!(
                "{p}: the head's [programs.{p}] (repo {:?}) and its pin are carried as they \
                 stand, and every client keeps {p} on its vendor lane — only `--pin {p}=<b>` \
                 naming a verified ALab pack switches the row to repo {:?}",
                block.repo,
                programs::get(p).map_or("", |d| d.repo)
            )
        } else if atpkg::vendor_direct::is_vendor(p) {
            format!(
                "{p} is vendor-direct — its head pin and block are carried until --retire {p} \
                 drops them"
            )
        } else {
            format!(
                "{p} is not a row of programs::PROGRAMS — its block is carried from the head; \
                 add the row (or retire it)"
            )
        });
        programs_out.push((p.to_string(), block));
    }
    for (p, block) in &programs_out {
        if block.names_requires() {
            return Err(Error::refused(format!(
                "[programs.{p}] names the retired `requires` — every client refuses that \
                 program; give it a table row"
            )));
        }
        if let Some(old) = base.programs.get(p)
            && describe(old) != describe(block)
        {
            log.say(format!(
                "metadata: {p}: {} -> {}",
                describe(old),
                describe(block)
            ));
        }
    }

    let index_build = base.index_build + 1;
    let pairs = |v: &[(String, Build)]| {
        if v.is_empty() {
            "<none>".to_string()
        } else {
            v.iter()
                .map(|(p, b)| format!("{p}@{b}"))
                .collect::<Vec<_>>()
                .join(" ")
        }
    };
    log.say(format!(
        "index_build {index_build} (head {} + 1)",
        base.index_build
    ));
    log.say(format!(
        "carry-forward (unchanged build, the published pack reused): {}",
        pairs(&carried)
    ));
    log.say(format!(
        "updated (new build, must be public before the index is): {}",
        if moved.is_empty() {
            "<none>".to_string()
        } else {
            moved
                .iter()
                .map(|m| format!("{}@{}->{}", m.program, m.from, m.to))
                .collect::<Vec<_>>()
                .join(" ")
        }
    ));
    log.say(format!("new programs (not in the head): {}", pairs(&added)));
    if !retired.is_empty() {
        log.note(format!(
            "retiring head program(s) as asked: {} — installed copies keep their last shims, \
             so retire on purpose only",
            retired.join(" ")
        ));
    }
    if moved.is_empty() && added.is_empty() && delta.target_pins.is_empty() {
        log.note("no pin moves: only the index number and the horizon move");
    }

    Ok(Plan {
        index_build,
        valid_until: valid_until.to_string(),
        machine_id: machine_id.to_string(),
        roster_seq,
        channel: Channel {
            name: head.name.clone(),
            channel_build: index_build,
            min_build,
            min_build_by_program,
            yanked: yanked.iter().map(yank_entry).collect(),
            pin,
            pin_by_target: overlays,
        },
        programs: programs_out,
        moved,
        added,
        carried,
        retired,
    })
}

/// A TOML key: bare when it can be, quoted otherwise.
fn key(k: &str) -> String {
    if !k.is_empty()
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
    {
        k.to_string()
    } else {
        string(k)
    }
}

/// A TOML basic string.
fn string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04X}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `pin` in the order the program blocks render, then anything else sorted.
fn pin_line(plan: &Plan) -> String {
    let order: Vec<&str> = plan.programs.iter().map(|(p, _)| p.as_str()).collect();
    let mut cells: Vec<String> = order
        .iter()
        .filter_map(|p| {
            plan.channel
                .pin
                .get(*p)
                .map(|b| format!("{} = {b}", key(p)))
        })
        .collect();
    cells.extend(
        plan.channel
            .pin
            .iter()
            .filter(|(p, _)| !order.contains(&p.as_str()))
            .map(|(p, b)| format!("{} = {b}", key(p))),
    );
    format!("{{ {} }}", cells.join(", "))
}

fn inline(map: &BTreeMap<String, Build>) -> String {
    format!(
        "{{ {} }}",
        map.iter()
            .map(|(p, b)| format!("{} = {b}", key(p)))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// The signed `index.toml`, byte for byte.
#[must_use]
pub fn render(plan: &Plan) -> String {
    let mut s = String::from(
        "# Generated by `atpkg-publish index` — the MACHINE-signed allow-list.\n\
         # Authorized by the master-signed roster published beside it; the signature covers\n\
         # the exact bytes, so DO NOT edit by hand.\n",
    );
    let _ = writeln!(s, "schema = 2");
    let _ = writeln!(s, "index_build = {}", plan.index_build);
    let _ = writeln!(s, "valid_until = {}", string(&plan.valid_until));
    let _ = writeln!(s, "machine_id = {}", string(&plan.machine_id));
    let _ = writeln!(s, "roster_seq = {}", plan.roster_seq);
    for (name, p) in &plan.programs {
        let _ = writeln!(s, "\n[programs.{}]", key(name));
        let _ = writeln!(s, "repo = {}", string(&p.repo));
        let _ = writeln!(s, "policy = {}", string(&p.policy));
        for (k, v) in [
            ("coherence_group", &p.coherence_group),
            ("system", &p.system),
            ("unavailable_hint", &p.unavailable_hint),
        ] {
            if let Some(v) = v {
                let _ = writeln!(s, "{k} = {}", string(v));
            }
        }
    }
    let c = &plan.channel;
    let _ = writeln!(s, "\n[[channels]]");
    let _ = writeln!(s, "name = {}", string(&c.name));
    let _ = writeln!(s, "channel_build = {}", c.channel_build);
    let _ = writeln!(s, "min_build = {}", c.min_build);
    if !c.min_build_by_program.is_empty() {
        let _ = writeln!(
            s,
            "min_build_by_program = {}",
            inline(&c.min_build_by_program)
        );
    }
    if !c.yanked.is_empty() {
        let _ = writeln!(
            s,
            "yanked = [{}]",
            c.yanked
                .iter()
                .map(|y| string(y))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let _ = writeln!(s, "pin = {}", pin_line(plan));
    if !c.pin_by_target.is_empty() {
        let _ = writeln!(
            s,
            "pin_by_target = {{ {} }}",
            c.pin_by_target
                .iter()
                .map(|(t, cells)| format!("{} = {}", string(t), inline(cells)))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    s
}

/// A roster pair this publish could ship, verified under the paper master.
#[derive(Clone, Debug)]
pub struct RosterPair {
    /// Where it was found.
    pub path: PathBuf,
    /// Its exact bytes.
    pub bytes: Vec<u8>,
    /// The master's signature over them.
    pub sig: Vec<u8>,
    /// Parsed.
    pub roster: aterm_update_core::Roster,
}

/// The newest master-verified roster pair among `candidates` (each `<path>` beside its `<path>.sig`); the head's own pair is one, so the generation shipped is never below the head's.
/// # Errors
/// A refusal when no candidate verifies, or at a fork.
pub fn newest_roster(ctx: &Ctx, candidates: &[PathBuf], log: &mut Log) -> Result<RosterPair> {
    let anchor: Vec<&str> = ctx.anchor.iter().map(String::as_str).collect();
    let mut best: Option<RosterPair> = None;
    let mut fork: Option<PathBuf> = None;
    for path in candidates {
        let mut sig_path = path.as_os_str().to_owned();
        sig_path.push(".sig");
        let (Ok(bytes), Ok(sig)) = (std::fs::read(path), std::fs::read(PathBuf::from(sig_path)))
        else {
            continue;
        };
        let verdict = aterm_update_core::verify_roster(&anchor, bytes.clone(), &sig)
            .and_then(|v| aterm_update_core::Roster::parse(&v))
            .and_then(|r| r.admit(0, ctx.now_unix).map(|()| r));
        let roster = match verdict {
            Ok(r) => r,
            Err(why) => {
                log.note(format!(
                    "skipped the roster pair at {}: it does not verify under the paper master, \
                     or has lapsed ({why:?})",
                    ctx.shown(path)
                ));
                continue;
            }
        };
        match &best {
            Some(b) if roster.roster_seq < b.roster.roster_seq => {}
            Some(b) if roster.roster_seq == b.roster.roster_seq => {
                if bytes != b.bytes {
                    fork = Some(path.clone());
                }
            }
            _ => {
                fork = None;
                best = Some(RosterPair {
                    path: path.clone(),
                    bytes,
                    sig,
                    roster,
                });
            }
        }
    }
    let best = best.ok_or_else(|| {
        Error::refused(format!(
            "no master-signed roster pair among {} — copy aterm-machines.toml and its .sig \
             from a machine that has them, or from the newest atpkg-index-N release on {}. \
             Never start a second roster",
            candidates
                .iter()
                .map(|c| ctx.shown(c))
                .collect::<Vec<_>>()
                .join(", "),
            ctx.index_slug()
        ))
    })?;
    if let Some(other) = fork {
        return Err(Error::refused(format!(
            "two DIFFERENT rosters both claim roster_seq {} ({} and {}) — a lineage fork: the \
             paper master signed two documents at one generation. Settle which is canonical \
             before publishing anything under either",
            best.roster.roster_seq,
            ctx.shown(&best.path),
            ctx.shown(&other)
        )));
    }
    Ok(best)
}

/// What a publish did.
#[derive(Debug)]
pub struct Outcome {
    /// The trait's answer.
    pub published: Published,
    /// The index it signed.
    pub plan: Plan,
}

fn within(e: Error, what: &str) -> Error {
    match e {
        Error::Refused(m) => Error::Refused(format!("{what}: {m}")),
        Error::CannotRun(m) => Error::CannotRun(format!("{what}: {m}")),
        Error::Usage(m) => Error::Usage(format!("{what}: {m}")),
    }
}

/// [`Indexer::publish`], its transcript in `log`.
/// # Errors
/// Every refusal the plan, the self-check, the triple gate, the mirror or the compare-and-swap raises; nothing is published after one.
pub fn publish_with(ctx: &Ctx, request: &IndexRequest, log: &mut Log) -> Result<Outcome> {
    let out = &request.out;
    if ctx.anchor.is_empty() {
        return Err(Error::refused(
            "pins::PAPER_MASTER_PUBKEYS is EMPTY — this build trusts no paper master, so \
             nothing it signs could be verified by any client built from it (arm it with \
             `atpkg-keys setup --id <id>` on the first machine)",
        ));
    }
    if request.upload {
        let ok = std::fs::metadata(&ctx.tokens.channel).is_ok_and(|m| m.len() > 0);
        if !ok {
            return Err(Error::refused(format!(
                "--upload needs the public owner's token at {} — the index and the packs it \
                 pins are published to {}",
                ctx.shown(&ctx.tokens.channel),
                ctx.accounts.public
            )));
        }
    }
    let head_dir = Scratch::new("head")?;

    let (base, head) = read_baseline(ctx, request, head_dir.path(), log)?;

    if let Some(q) = &head
        && q.lapsed(ctx.now_unix)
    {
        log.warn(format!(
            "the baseline atpkg-index-{} LAPSED at {}: every client refuses the channel until \
             an index with a future horizon is published",
            q.index.index_build, q.index.valid_until
        ));
    }

    let mut candidates = Vec::new();
    if let Some(q) = &head {
        candidates.push(q.file(QUAD_FILES[2]));
    }
    candidates.push(ctx.repo_root.join("dist").join(QUAD_FILES[2]));
    candidates.push(ctx.identity.roster.clone());
    let roster = newest_roster(ctx, &candidates, log)?;
    let signer = signer_for(ctx, &roster.roster)?;

    let valid_until = match (&request.delta.valid_until, &base.valid_until) {
        (Some(v), _) | (None, Some(v)) => v.clone(),
        (None, None) => {
            return Err(Error::usage(
                "a first publish names its --valid-until (there is no head to carry it from)",
            ));
        }
    };
    let remedy = if request.delta.valid_until.is_some() {
        "pass a --valid-until in the future"
    } else {
        "this publish carries the head's horizon: re-sign it with --valid-until \
         <YYYY-MM-DDTHH:MM:SSZ in the future>"
    };
    let until = horizon("the index's", &valid_until, ctx.now_unix, remedy)?;
    if until - ctx.now_unix < HORIZON_WARN_SECS {
        log.warn(format!(
            "the index's horizon {valid_until} is under 30 days away: every client refuses it \
             from then, and nothing re-signs it automatically"
        ));
    }
    let roster_until = horizon(
        "the roster's",
        &roster.roster.valid_until,
        ctx.now_unix,
        "re-sign the roster with the paper master (`atpkg-keys join` or `machine-revoke`)",
    )?;
    if roster_until < until {
        log.warn(format!(
            "the roster expires {}, before this index ({valid_until}): clients refuse the index \
             from that day. Any roster edit (`atpkg-keys join` or `machine-revoke`, with the \
             paper master) re-signs it; the next publish then ships the new pair",
            roster.roster.valid_until
        ));
    }
    log.say(format!(
        "publishing as machine '{}' under roster_seq {} ({})",
        signer.record.id,
        signer.roster_seq,
        ctx.shown(&roster.path)
    ));
    log.say(format!(
        "horizon — index {valid_until}, roster {}",
        roster.roster.valid_until
    ));

    let plan = plan(
        &base,
        &request.delta,
        &signer.record.id,
        signer.roster_seq,
        &valid_until,
        log,
    )?;

    std::fs::create_dir_all(out).map_err(|e| Error::io(format!("mkdir {}", out.display()), &e))?;
    let _lock = aterm_update_core::FileLock::acquire_within(
        &out.join(".atpkg-publish.lock"),
        Duration::ZERO,
    )
    .map_err(|e| {
        Error::refused(format!(
            "another publish is writing {} ({e}) — wait for it, or use another --out",
            ctx.shown(out)
        ))
    })?;
    let new = write_quad(ctx, &plan, &roster, &signer.key, out, log)?;
    for (def, b) in alab_pins(&plan) {
        if let Err(e) = crate::artifact::package::verify_pinned(
            ctx,
            &new,
            def,
            b,
            request.pack_dir.as_deref(),
            log,
        ) {
            for f in QUAD_FILES {
                let _ = std::fs::remove_file(out.join(f));
            }
            return Err(within(
                e,
                &format!(
                    "REFUSING to pin {}@{b}: its pack does not verify as a client stages it — \
                     the signed quad was removed from {}, nothing was published",
                    def.name,
                    ctx.shown(out)
                ),
            ));
        }
    }

    if request.upload {
        publish_quad(ctx, request, &base, head.as_ref(), &new, &plan, log)?;
    } else {
        log.say("DRY RUN — nothing published (--upload publishes)");
    }
    Ok(Outcome {
        published: Published {
            index_build: plan.index_build,
            dir: out.clone(),
            uploaded: request.upload,
        },
        plan,
    })
}

/// Read the verified baseline without touching the output. The caller owns `head_dir`
/// until publication finishes, because the triple gate reads the head's signed files.
fn read_baseline(
    ctx: &Ctx,
    request: &IndexRequest,
    head_dir: &Path,
    log: &mut Log,
) -> Result<(Base, Option<VerifiedQuad>)> {
    let slug = ctx.index_slug();
    let out = &request.out;
    Ok(match &request.baseline {
        Baseline::ChannelHead => {
            let (n, q) = quad::fetch_head_with(ctx, &slug, head_dir, Freshness::Ignore)?;
            log.say(format!(
                "baseline: the channel head {}, verified",
                index_tag(n)
            ));
            (Base::of_head(&q)?, Some(q))
        }
        Baseline::Dir(dir) => {
            if dir.canonicalize().ok() == out.canonicalize().ok() {
                return Err(Error::usage(
                    "the baseline quad and --out are one directory: the new quad would \
                     overwrite the one it is built over",
                ));
            }
            let q = quad::verify_dir_with(ctx, dir, Freshness::Ignore)?;
            log.say(format!(
                "baseline: {} (atpkg-index-{}), verified",
                ctx.shown(dir),
                q.index.index_build
            ));
            (Base::of_head(&q)?, Some(q))
        }
        Baseline::First {
            channel,
            index_build,
        } => {
            if request.upload {
                match ctx.hosts.channel.releases(&slug) {
                    Ok(releases) => {
                        if let Some(n) = quad::head_number(&releases) {
                            return Err(Error::refused(format!(
                                "a FIRST publish of a channel {slug} already serves \
                                 (atpkg-index-{n} is published) — baseline on its head"
                            )));
                        }
                    }
                    Err(HostError::NotFound) => {}
                    Err(e) => return Err(host_error(format!("list the releases of {slug}"), &e)),
                }
            }
            log.warn(format!(
                "a FIRST publish of channel {channel}: no carry, no downgrade refusal, no floor"
            ));
            (Base::first(channel, *index_build)?, None)
        }
    })
}

/// Emit and self-verify the quad while the caller holds the output lock. A prior
/// unpublished quad is cleared only after the caller has accepted the complete plan.
fn write_quad(
    ctx: &Ctx,
    plan: &Plan,
    roster: &RosterPair,
    key: &crate::identity::MachineKey,
    out: &Path,
    log: &mut Log,
) -> Result<VerifiedQuad> {
    for f in QUAD_FILES {
        let _ = std::fs::remove_file(out.join(f));
    }

    let text = render(plan);
    let index_path = out.join(QUAD_FILES[0]);
    let write = |path: &Path, bytes: &[u8]| {
        std::fs::write(path, bytes).map_err(|e| Error::io(format!("write {}", path.display()), &e))
    };
    write(&index_path, text.as_bytes())?;
    write(&out.join(QUAD_FILES[1]), &key.sign(text.as_bytes())?)?;
    write(&out.join(QUAD_FILES[2]), &roster.bytes)?;
    write(&out.join(QUAD_FILES[3]), &roster.sig)?;
    let new = quad::verify_dir(ctx, out).map_err(|e| {
        within(
            e,
            "SELF-CHECK FAILED — refusing to publish an index no client would accept",
        )
    })?;
    if new.index.index_build != plan.index_build {
        return Err(Error::refused(
            "SELF-CHECK FAILED — the verified index does not carry the number this publish \
             planned",
        ));
    }
    log.say(format!(
        "wrote + signed {} as machine '{}' (self-verified through the client chain); the \
         roster pair rides beside it (roster_seq {})",
        ctx.shown(&index_path),
        plan.machine_id,
        plan.roster_seq
    ));

    Ok(new)
}

/// Make the packs public before the index, and keep the output locked through the
/// compare-and-swap and the final anonymous download-host confirmation.
fn publish_quad(
    ctx: &Ctx,
    request: &IndexRequest,
    base: &Base,
    head: Option<&VerifiedQuad>,
    new: &VerifiedQuad,
    plan: &Plan,
    log: &mut Log,
) -> Result<()> {
    let out = &request.out;
    let slug = ctx.index_slug();
    let tag = index_tag(plan.index_build);
    if head.is_some() {
        let base_tag = index_tag(base.index_build);
        match ctx
            .hosts
            .channel
            .probe_public(&slug, &base_tag, QUAD_FILES[0])
        {
            Probe::Present => {}
            Probe::Absent => {
                return Err(Error::refused(format!(
                    "REFUSING — the baseline {base_tag} is not published on {slug} (the \
                     download host answers 404): a draft, a deleted release, or a private \
                     repo. {tag} above it would leave a hole the clients' walk cannot cross; \
                     baseline on the channel's newest PUBLISHED index, or publish the draft \
                     first"
                )));
            }
            Probe::Unknown(why) => {
                return Err(Error::cannot_run(format!(
                    "could not ask the download host whether {base_tag} is published: {why}"
                )));
            }
        }
    }
    if let Some(h) = head {
        triple_gate(ctx, plan, h, new, &request.delta.triple_drops, out, log)?;
    }
    mirror::mirror(ctx, new, &MirrorOptions::publish(), log).map_err(|e| {
        within(
            e,
            &format!(
                "the packs {tag} pins are not all public on {} — nothing was published",
                ctx.accounts.public
            ),
        )
    })?;
    compare_and_swap(ctx, plan, out, log)?;
    match probe_served(&*ctx.hosts.channel, &slug, &tag, QUAD_FILES[0]) {
        Probe::Present => log.say(format!(
            "published {tag} to {slug} ({} answers)",
            slug.download_url(&tag, QUAD_FILES[0])
        )),
        other => {
            return Err(Error::refused(format!(
                "{tag} was published to {slug} but {} still answers {other:?} after \
                 {SERVE_PROBES} probes — is the repo public?",
                slug.download_url(&tag, QUAD_FILES[0])
            )));
        }
    }
    Ok(())
}

/// The release notes every index release carries: what it is, first, because these tags
/// interleave with the app's.
fn notes(plan: &Plan) -> String {
    format!(
        "Machine-readable package index for the aterm toolchain — not an app release. For \
         aterm itself, use the latest vX.Y.0 release.\n\nMachine-signed atpkg index (channel \
         {}, valid_until {}), published with the master-signed machine roster that authorizes \
         it (roster_seq {}, signed by machine {}). Authenticity is the Ed25519 chain from the \
         paper master — the host is never an authenticity input.",
        plan.channel.name, plan.valid_until, plan.roster_seq, plan.machine_id
    )
}

/// The compare-and-swap on `atpkg-index-N`: two publishers baselined on one head both mint N; whichever creates it first wins, and the other never clobbers it.
/// # Errors
/// A refusal for another publisher's bytes, a release with no index.toml, a create that failed (a racing publisher: re-run, never fall back to an upload), or a release whose state cannot be read.
pub fn compare_and_swap(ctx: &Ctx, plan: &Plan, out: &Path, log: &mut Log) -> Result<()> {
    let slug = ctx.index_slug();
    let host = &*ctx.hosts.channel;
    let tag = index_tag(plan.index_build);
    let files: Vec<PathBuf> = QUAD_FILES.iter().map(|f| out.join(f)).collect();
    let existing = match host.release(&slug, &tag) {
        Err(HostError::NotFound) => {
            host.create(
                &slug,
                &tag,
                &format!("atpkg index {}", plan.index_build),
                &notes(plan),
                &files,
            )
            .map_err(|e| {
                host_error(
                    format!(
                        "creating {tag} on {slug} failed — if it exists now (a racing \
                         publisher), re-run: an existing release is compared, never clobbered"
                    ),
                    &e,
                )
            })?;
            log.say(format!("created {tag} on {slug} (a prerelease)"));
            return Ok(());
        }
        Err(e) => {
            return Err(host_error(
                format!(
                    "could not read whether the existing {tag} on {slug} is a DRAFT — refusing; \
                     re-run, or look at it"
                ),
                &e,
            ));
        }
        Ok(r) => r,
    };
    let draft_note = if existing.draft {
        " It is an orphaned DRAFT (a publish killed between create and publish leaves one)."
    } else {
        ""
    };
    if !existing.assets.iter().any(|a| a == QUAD_FILES[0]) {
        return Err(Error::refused(format!(
            "{tag} exists on {slug} without an index.toml — a release mid-upload, or \
             half-created; never clobbered. Look at it before re-running.{draft_note}"
        )));
    }
    let present: Vec<&str> = QUAD_FILES
        .iter()
        .copied()
        .filter(|f| existing.assets.iter().any(|a| a == f))
        .collect();
    let scratch = Scratch::new("cas")?;
    let cmp = scratch.path();
    download_retried(host, &slug, &tag, &present, cmp).map_err(|e| {
        host_error(
            format!(
                "could not download the existing {tag} from {slug} to compare it — refusing \
                 (never clobbered)"
            ),
            &e,
        )
    })?;
    for name in &present {
        let theirs = std::fs::read(cmp.join(name)).map_err(|e| Error::io(name, &e))?;
        let ours = std::fs::read(out.join(name)).map_err(|e| Error::io(name, &e))?;
        if theirs != ours {
            return Err(Error::refused(format!(
                "{tag} already exists on {slug} with DIFFERENT bytes ({name}) — another \
                 publisher took index {}; never clobbered. Re-baseline on it (publish again \
                 over the channel head) and publish again.{draft_note}",
                plan.index_build
            )));
        }
    }
    let missing: Vec<PathBuf> = QUAD_FILES
        .iter()
        .filter(|f| !present.contains(f))
        .map(|f| out.join(f))
        .collect();
    if missing.is_empty() {
        log.say(format!(
            "{tag} is already on {slug}, byte-identical — nothing to upload"
        ));
    } else {
        host.upload(&slug, &tag, &missing, Replace::Never)
            .map_err(|e| host_error(format!("uploading the missing quad files to {tag}"), &e))?;
        log.say(format!(
            "{tag} was already on {slug} with identical bytes — uploaded the missing file(s)"
        ));
    }
    if existing.draft {
        host.publish_draft(&slug, &tag).map_err(|e| {
            host_error(
                format!(
                    "{tag} on {slug} is an orphaned DRAFT carrying exactly these bytes and \
                     publishing it failed"
                ),
                &e,
            )
        })?;
        log.say(format!(
            "{tag} was an orphaned DRAFT with identical bytes — published it (still a \
             prerelease)"
        ));
    }
    Ok(())
}

/// The darwin targets of a verified manifest.
fn darwin_targets(pkg: &atpkg::PkgManifest) -> BTreeSet<String> {
    pkg.artifacts
        .iter()
        .filter(|a| Triple::parse(&a.target).is_ok_and(|t| t.os() == Os::Darwin))
        .map(|a| a.target.clone())
        .collect()
}

/// A manifest pair read where a client reads it (the public owner's download host), else
/// from the staging owner, and verified under `quad`.
fn read_manifest(
    ctx: &Ctx,
    quad: &VerifiedQuad,
    repo: &str,
    program: &str,
    build: Build,
    staging_too: bool,
    dir: &Path,
) -> Option<atpkg::PkgManifest> {
    let tag = upload::tag(program, build);
    let mf = pkgdoc::manifest_name(program, build);
    let sig = pkgdoc::sig_name(program, build);
    let public = ctx.public_slug(repo);
    let host = &*ctx.hosts.channel;
    let public_dir = dir.join("public").join(&tag);
    let got_public = host.fetch_public(&public, &tag, &mf, &public_dir.join(&mf), MANIFEST_CAP)
        == Probe::Present
        && host.fetch_public(&public, &tag, &sig, &public_dir.join(&sig), MANIFEST_CAP)
            == Probe::Present;
    let from = if got_public {
        public_dir
    } else if staging_too {
        let staging_dir = dir.join("staging").join(&tag);
        download_retried(
            &*ctx.hosts.staging,
            &ctx.staging_slug(repo),
            &tag,
            &[&mf, &sig],
            &staging_dir,
        )
        .ok()?;
        staging_dir
    } else {
        return None;
    };
    let bytes = std::fs::read(from.join(&mf)).ok()?;
    let sig = std::fs::read(from.join(&sig)).ok()?;
    quad.verify_pkg(&bytes, &sig).ok()
}

/// A pin may not silently drop a darwin target: clients on a target the previous pin
/// served would stay on that build with nothing said.
fn triple_gate(
    ctx: &Ctx,
    plan: &Plan,
    head: &VerifiedQuad,
    new: &VerifiedQuad,
    drops: &[TripleDrop],
    out: &Path,
    log: &mut Log,
) -> Result<()> {
    let dir = out.join(".triple-gate");
    let _ = std::fs::remove_dir_all(&dir);
    let result = (|| {
        for m in &plan.moved {
            // the vendor.
            if programs::alab_package(&m.program).is_some() {
                continue;
            }
            let repo = plan
                .programs
                .iter()
                .find(|(p, _)| *p == m.program)
                .map(|(_, b)| b.repo.clone())
                .unwrap_or_default();
            let old = read_manifest(ctx, head, &repo, &m.program, m.from, false, &dir).ok_or_else(
                || {
                    Error::refused(format!(
                        "cannot read {}'s head manifest (atpkg-{}-{} on {}) to check the darwin \
                         targets it serves — refusing rather than guessing",
                        m.program,
                        m.program,
                        m.from,
                        ctx.public_slug(&repo)
                    ))
                },
            )?;
            let new_pkg =
                read_manifest(ctx, new, &repo, &m.program, m.to, true, &dir).ok_or_else(|| {
                    Error::refused(format!(
                        "cannot read {}'s new manifest (atpkg-{}-{}) on either owner to check the \
                         darwin targets it serves — refusing rather than guessing",
                        m.program, m.program, m.to
                    ))
                })?;
            let kept = darwin_targets(&new_pkg);
            for t in darwin_targets(&old).difference(&kept) {
                if let Some(d) = drops
                    .iter()
                    .find(|d| d.program == m.program && d.triple.as_str() == t)
                {
                    log.say(format!(
                        "{} {}->{} drops {t} (allowed): {}",
                        m.program, m.from, m.to, d.reason
                    ));
                    continue;
                }
                return Err(Error::refused(format!(
                    "REFUSING — {p} {f}->{to} drops {t}: build {f} served it and build {to} has \
                     no row for it, so every {t} client stays on {f} with nothing said. Publish \
                     a {t} row for {to}, or name the drop: --allow-triple-drop {p}={t} --reason \
                     '<why>'",
                    p = m.program,
                    f = m.from,
                    to = m.to
                )));
            }
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// The index verb's own flags, beside the delta.
#[derive(Debug, Default)]
struct Options {
    out: Option<PathBuf>,
    upload: bool,
    baseline: Option<PathBuf>,
    first: Option<String>,
    first_build: Option<u64>,
    pack_dir: Option<PathBuf>,
}

/// The change a command line names (`--pin name=build`, `--target-pin <triple>:name=build[,…]`, `--yank`, `--unyank`, `--min-build`, `--lower-min-build`, `--retire`, `--retire-target <triple>[:name]`, `--allow-triple-drop name=<triple> --reason …`, `--valid-until`).
/// # Errors
/// A usage error for an unknown flag, a malformed value, a repeated `(target, program)`, or a triple drop without a reason.
pub fn parse_delta(args: &[String]) -> Result<Delta> {
    parse(args, false).map(|(d, _)| d)
}

fn parse(args: &[String], verb: bool) -> Result<(Delta, Options)> {
    let mut d = Delta::default();
    let mut o = Options::default();
    let mut drops: Vec<(String, Triple)> = Vec::new();
    let mut reason: Option<String> = None;
    let mut i = 0;
    let build = |what: &str, s: &str| -> Result<Build> {
        if s.is_empty() || !s.chars().all(|c| c.is_ascii_digit()) {
            return Err(Error::usage(format!(
                "{what}: the build must be decimal digits, got `{s}`"
            )));
        }
        s.parse()
            .map_err(|_| Error::usage(format!("{what}: `{s}` is out of range")))
    };
    let name_build = |flag: &str, v: &str| -> Result<(String, Build)> {
        let (n, b) = v
            .split_once('=')
            .ok_or_else(|| Error::usage(format!("{flag} {v}: expected <name>=<build>")))?;
        if n.is_empty() {
            return Err(Error::usage(format!("{flag} {v}: no program name")));
        }
        Ok((n.to_string(), build(&format!("{flag} {v}"), b)?))
    };
    while i < args.len() {
        let flag = args[i].as_str();
        match flag {
            "--pin" => {
                let (p, b) = name_build(flag, flag_value(args, &mut i, flag)?)?;
                if d.pins.insert(p.clone(), b).is_some() {
                    return Err(Error::usage(format!("--pin names {p} twice")));
                }
            }
            "--target-pin" => {
                let v = flag_value(args, &mut i, flag)?;
                let (t, cells) = v.split_once(':').ok_or_else(|| {
                    Error::usage(format!(
                        "--target-pin {v}: expected <triple>:<name>=<build>[,…]"
                    ))
                })?;
                let triple = Triple::parse(t)?;
                for cell in cells.split(',') {
                    let (p, b) = name_build(flag, cell)?;
                    if d.target_pins
                        .entry(triple)
                        .or_default()
                        .insert(p.clone(), b)
                        .is_some()
                    {
                        return Err(Error::usage(format!(
                            "--target-pin pins {p} on {triple} twice — one build per program per \
                             target"
                        )));
                    }
                }
            }
            "--yank" => {
                d.yank
                    .insert(parse_yank(flag_value(args, &mut i, flag)?, true)?);
            }
            "--unyank" => {
                d.unyank
                    .insert(parse_yank(flag_value(args, &mut i, flag)?, true)?);
            }
            "--min-build" => {
                d.min_build = Some(build(flag, flag_value(args, &mut i, flag)?)?);
            }
            "--lower-min-build" => d.lower_min_build = true,
            "--retire" => {
                d.retire.insert(flag_value(args, &mut i, flag)?.to_string());
            }
            "--retire-target" => {
                let v = flag_value(args, &mut i, flag)?;
                let (t, p) = match v.split_once(':') {
                    Some((t, p)) if !p.is_empty() => (t, Some(p.to_string())),
                    Some(_) => {
                        return Err(Error::usage(format!("--retire-target {v}: no program")));
                    }
                    None => (v, None),
                };
                d.retire_target.insert((Triple::parse(t)?, p));
            }
            "--allow-triple-drop" => {
                let v = flag_value(args, &mut i, flag)?;
                let (p, t) = v.split_once('=').ok_or_else(|| {
                    Error::usage(format!("--allow-triple-drop {v}: expected <name>=<triple>"))
                })?;
                drops.push((p.to_string(), Triple::parse(t)?));
            }
            "--reason" => reason = Some(flag_value(args, &mut i, flag)?.to_string()),
            "--valid-until" => {
                d.valid_until = Some(flag_value(args, &mut i, flag)?.to_string());
            }
            "--out" if verb => o.out = Some(PathBuf::from(flag_value(args, &mut i, flag)?)),
            "--upload" if verb => o.upload = true,
            "--baseline" if verb => {
                o.baseline = Some(PathBuf::from(flag_value(args, &mut i, flag)?));
            }
            "--first" if verb => o.first = Some(flag_value(args, &mut i, flag)?.to_string()),
            "--pack-dir" if verb => {
                o.pack_dir = Some(PathBuf::from(flag_value(args, &mut i, flag)?));
            }
            "--index-build" if verb => {
                let v = flag_value(args, &mut i, flag)?;
                o.first_build = Some(v.parse().map_err(|_| {
                    Error::usage(format!("--index-build {v}: expected a positive number"))
                })?);
            }
            other => {
                return Err(Error::usage(format!(
                    "index: unknown argument `{other}` (atpkg-publish help)"
                )));
            }
        }
        i += 1;
    }
    let reason = reason.filter(|r| !r.trim().is_empty());
    match (drops.is_empty(), reason) {
        (true, None) => {}
        (true, Some(_)) => {
            return Err(Error::usage(
                "--reason explains an --allow-triple-drop, and none was given",
            ));
        }
        (false, None) => {
            return Err(Error::usage(
                "--allow-triple-drop needs --reason — say why every client on that target is \
                 held on the old build",
            ));
        }
        (false, Some(reason)) => {
            d.triple_drops = drops
                .into_iter()
                .map(|(program, triple)| TripleDrop {
                    program,
                    triple,
                    reason: reason.clone(),
                })
                .collect();
        }
    }
    Ok((d, o))
}

/// The request a command line names: [`parse_delta`]'s flags, plus `--out DIR` (default `dist`), `--upload`, `--baseline DIR` (a verified quad on disk instead of the channel head), and `--first <channel> --index-build N` (a channel's genuine first publish).
/// # Errors
/// A usage error for a bad command line.
pub fn parse_request(args: &[String]) -> Result<IndexRequest> {
    let (delta, o) = parse(args, true)?;
    let baseline = match (o.first, o.first_build, o.baseline) {
        (Some(channel), Some(index_build), None) => Baseline::First {
            channel,
            index_build,
        },
        (None, None, Some(dir)) => Baseline::Dir(dir),
        (None, None, None) => Baseline::ChannelHead,
        (Some(_), Some(_), Some(_)) => {
            return Err(Error::usage(
                "--first and --baseline are two different baselines",
            ));
        }
        _ => {
            return Err(Error::usage(
                "a first publish is --first <channel> --index-build <n>, both",
            ));
        }
    };
    if o.upload && o.pack_dir.is_some() {
        return Err(Error::usage(
            "--pack-dir verifies an ALab pack from a local directory for a dry run; a publish \
             verifies the pack the hosts serve, which every client fetches",
        ));
    }
    Ok(IndexRequest {
        baseline,
        delta,
        out: o.out.unwrap_or_else(|| PathBuf::from("dist")),
        upload: o.upload,
        pack_dir: o.pack_dir,
    })
}

/// `atpkg-publish index …` ([`parse_request`]).
/// # Errors
/// A usage error for a bad command line; whatever the publish refuses.
pub fn run(ctx: &Ctx, args: &[String]) -> Result<()> {
    let request = parse_request(args)?;
    let mut log = Log::echo("atpkg-publish index");
    let outcome = publish_with(ctx, &request, &mut log)?;
    log.say(format!(
        "{} atpkg-index-{} ({})",
        if outcome.published.uploaded {
            "published"
        } else {
            "signed (not published)"
        },
        outcome.published.index_build,
        ctx.shown(&outcome.published.dir)
    ));
    Ok(())
}
