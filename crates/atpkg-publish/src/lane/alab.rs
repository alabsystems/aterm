// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The Apple lane: keep the ALab programs' pins at each repo's newest signed source
//! release, unattended, on a rostered Mac (launchd, every 6 h).
//!
//! Per run, over the verified channel head (a head or pin below what this lane last saw
//! refuses the run): a pure [`decide`] per program row (plain → tracked, bundle → human
//! lane; a vendor-direct program has no row, so the lane never reads it); the packs,
//! aarch64 first and every other Apple target merged onto a copy; the row gate (a
//! half-packed release is never pinned); the coherence hold ([`super::hold`]); and one
//! index whose delta is the ready builds. For trust, the gap to main is reported
//! ([`super::gap`]).
//!
//! What is built is what was signed: the lane builds beside the machine key, so it builds
//! only the dev commit the publication engine's ledger maps to the public commit a
//! release's `SHA256SUMS` names, that SUMS verified under the head's roster. A dev tag,
//! which nothing signs, is never read.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::LaneKind;
use super::gap::{self, MainGap, PinCommit};
use super::hold::Holds;
use super::report::{self, Notifier, Transcript};
use super::rows::{self, Fetched};
use super::state::{LaneState, Lock};
use crate::api::{Baseline, Build, Delta, IndexRequest, Indexer, PackRequest, Packer, Signing};
use crate::ctx::Ctx;
use crate::error::{Error, Result};
use crate::host::{Probe, Release};
use crate::programs::{self, Hello, ProgramDef, Shape};
use crate::quad::{self, VerifiedQuad};
use crate::triple::{AARCH64_APPLE_DARWIN, Os, Triple};

/// The transcript's prefix.
pub const ME: &str = "auto-alab";

/// The notification title.
pub const TITLE: &str = "aterm auto-alab";

/// Pack failures of one build before it is latched.
pub const BLOCK_AFTER: u32 = 3;

/// An index horizon this close is warned about.
pub const HORIZON_WARN_DAYS: i64 = 30;

/// Consecutive unreadable heads before a person is told.
pub const HEAD_FAILURES_NOTIFY: u64 = 6;

/// The Apple targets: one build carries both rows, so the pins are resolved for both
/// (and a program the two resolve to different builds is refused, not collapsed).
pub const TARGETS: &[Triple] = programs::DUAL_DARWIN;

/// How one run is told to behave.
#[derive(Clone, Debug)]
pub struct Options {
    /// Decide and print, and change nothing: no clone, build, sign, upload, state or
    /// notification (an existing scratch clone is read but not fetched).
    pub dry_run: bool,
    /// Pack failures of one build before it is latched.
    pub block_after: u32,
    /// The publication engine's ledger (`~/publication/mappings.json`).
    pub ledger: PathBuf,
}

impl Options {
    /// The production defaults for `ctx`.
    #[must_use]
    pub fn new(ctx: &Ctx, dry_run: bool) -> Self {
        Self {
            dry_run,
            block_after: BLOCK_AFTER,
            ledger: ctx.home.join("publication/mappings.json"),
        }
    }
}

/// What the lane reaches beyond the context.
pub struct Deps<'a> {
    /// Packs plain programs.
    pub packer: &'a dyn Packer,
    /// Publishes the index.
    pub indexer: &'a dyn Indexer,
    /// Tells a person.
    pub notifier: &'a dyn Notifier,
    /// The pin's commit, as its installed compiler reports it ([`installed_pin_commit`]).
    pub pin_commit: &'a dyn Fn(&Ctx, &str, Build) -> Option<PinCommit>,
}

/// A signed source release, resolved to the dev commit that is built.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedRelease {
    /// `vX.Y.Z`.
    pub tag: String,
    /// The public commit its signed SUMS names.
    pub public_commit: String,
    /// The dev commit the engine ledger maps that to.
    pub dev_commit: String,
    /// Its build.
    pub build: Build,
}

/// Why a program's newest signed release is not usable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReleaseGap {
    /// No signed release could be read.
    NoTag(String),
    /// The release does not verify.
    Unverified {
        /// The tag.
        tag: String,
        /// The verifier's refusal.
        why: String,
    },
    /// It verified, and no dev commit could be bound to it.
    Unresolved {
        /// The tag.
        tag: String,
        /// The gap.
        why: String,
    },
}

/// What the staging owner holds for a release above the pin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Staging {
    /// Not looked up: the release is not above the pin.
    NotLooked,
    /// Looked up.
    Known {
        /// The rows the new build must carry: what its pack ships, plus every Apple row the
        /// pinned manifest carries.
        required: BTreeSet<Triple>,
        /// The rows `atpkg-<program>-<build>` carries there, when it exists.
        staged: Option<BTreeSet<Triple>>,
    },
    /// Nothing is known (unreachable, or a manifest that does not verify) — never read as
    /// absent: a transient failure must not demote a staged build to a re-pack.
    Unknown(String),
}

/// Everything [`decide`] reads about one program.
#[derive(Clone, Debug)]
pub struct Facts {
    /// The program.
    pub program: &'static ProgramDef,
    /// Its pin on the Apple targets.
    pub pin: Option<Build>,
    /// Its newest signed release, resolved.
    pub release: std::result::Result<SignedRelease, ReleaseGap>,
    /// What staging holds for it.
    pub staging: Staging,
    /// Whether its build is latched BLOCKED.
    pub latched: bool,
}

/// One program's decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Nothing decided this run.
    Skip(String),
    /// The release does not verify.
    Unverified(String),
    /// No dev commit binds to it.
    Unresolved(String),
    /// The pin is the newest signed release.
    Ok,
    /// The pin is ahead of it: a pack cut past the last promote (not a rollback).
    Ahead,
    /// Staging already holds the build with every required row: the index remains.
    Index,
    /// A sysroot bundle: its pack command is printed, never run.
    HumanLane {
        /// The rows its pack must carry.
        required: BTreeSet<Triple>,
        /// What a staging release of it lacks.
        missing: BTreeSet<Triple>,
    },
    /// Latched after repeated pack failures.
    Blocked {
        /// What a staging release of it lacks.
        missing: BTreeSet<Triple>,
    },
    /// Pack it.
    Pack {
        /// What a staging release of it lacks (a re-pack converges it).
        missing: BTreeSet<Triple>,
    },
}

impl Decision {
    /// Whether the program can move this run (else its group holds).
    #[must_use]
    pub fn ready(&self) -> bool {
        matches!(
            self,
            Self::Ok | Self::Ahead | Self::Index | Self::Pack { .. }
        )
    }

    /// The table's first word.
    #[must_use]
    pub fn verdict(&self) -> &'static str {
        match self {
            Self::Skip(_) => "SKIP",
            Self::Unverified(_) => "UNVERIFIED",
            Self::Unresolved(_) => "UNRESOLVED",
            Self::Ok | Self::Ahead => "OK",
            Self::Index => "INDEX",
            Self::HumanLane { .. } => "HUMAN-LANE",
            Self::Blocked { .. } => "BLOCKED",
            Self::Pack { .. } => "PACK",
        }
    }
}

/// The decision for one program (pure).
#[must_use]
pub fn decide(f: &Facts) -> Decision {
    let Some(pin) = f.pin else {
        return Decision::Skip(String::new());
    };
    let r = match &f.release {
        Err(ReleaseGap::NoTag(why)) => return Decision::Skip(why.clone()),
        Err(ReleaseGap::Unverified { why, .. }) => return Decision::Unverified(why.clone()),
        Err(ReleaseGap::Unresolved { why, .. }) => return Decision::Unresolved(why.clone()),
        Ok(r) => r,
    };
    if r.build > pin
        && let Staging::Unknown(why) = &f.staging
    {
        return Decision::Skip(why.clone());
    }
    if r.build == pin {
        return Decision::Ok;
    }
    if r.build < pin {
        return Decision::Ahead;
    }
    let (required, staged) = match &f.staging {
        Staging::Known { required, staged } => (required.clone(), staged.clone()),
        _ => (f.program.darwin_triples().iter().copied().collect(), None),
    };
    let missing: BTreeSet<Triple> = staged
        .as_ref()
        .map(|s| required.difference(s).copied().collect())
        .unwrap_or_default();
    if staged.is_some() && missing.is_empty() {
        return Decision::Index;
    }
    if matches!(f.program.shape, Shape::Bundle(_)) {
        return Decision::HumanLane { required, missing };
    }
    if f.latched {
        return Decision::Blocked { missing };
    }
    Decision::Pack { missing }
}

/// One row of the decision table.
#[derive(Clone, Debug)]
pub struct Row {
    /// The program.
    pub program: &'static ProgramDef,
    /// Its pin.
    pub pin: Option<Build>,
    /// Its newest signed release, when one was read.
    pub release: std::result::Result<SignedRelease, ReleaseGap>,
    /// The decision.
    pub decision: Decision,
}

impl Row {
    fn tag(&self) -> Option<&str> {
        match &self.release {
            Ok(r) => Some(&r.tag),
            Err(ReleaseGap::Unverified { tag, .. } | ReleaseGap::Unresolved { tag, .. }) => {
                Some(tag)
            }
            Err(ReleaseGap::NoTag(_)) => None,
        }
    }

    fn build(&self) -> Option<Build> {
        self.release.as_ref().ok().map(|r| r.build)
    }

    /// The decision as the table says it.
    #[must_use]
    pub fn render(&self, head: u64, staging: &str, gap: Option<&MainGap>) -> String {
        let note = gap.map(MainGap::row_note).unwrap_or_default();
        let (tag, build) = (self.tag().unwrap_or("?"), self.build().unwrap_or(0));
        let lacks = |m: &BTreeSet<Triple>, how: &str| {
            if m.is_empty() {
                String::new()
            } else {
                format!(
                    " ({how} {})",
                    super::state::names(&m.iter().copied().collect::<Vec<_>>())
                )
            }
        };
        match &self.decision {
            Decision::Skip(_) if self.pin.is_none() => format!(
                "SKIP: not pinned by index-{head} (a new program is a human decision: pin it \
                 with `atpkg-publish index --pin {}=<build>`)",
                self.program.name
            ),
            Decision::Skip(why) => format!("SKIP: {why}"),
            Decision::Unverified(why) => format!("UNVERIFIED: {why}"),
            Decision::Unresolved(why) => format!("UNRESOLVED: {why}"),
            Decision::Ok => format!("OK: pin {} is {tag}{note}", self.pin.unwrap_or(0)),
            Decision::Ahead => format!(
                "OK: pin {} is AHEAD of the newest signed release {tag} (build {build}) — a pack \
                 cut past the last promote{note}",
                self.pin.unwrap_or(0)
            ),
            Decision::Index => format!(
                "INDEX: {tag} (build {build}) is already on {staging} as {} — publishing the \
                 index is what remains",
                rows::tag(self.program.name, build)
            ),
            Decision::HumanLane { missing, .. } => format!(
                "HUMAN-LANE: {tag} (build {build}) > pin {} — a sysroot bundle; its pack \
                 command is below, never run here{}",
                self.pin.unwrap_or(0),
                lacks(missing, "the staging release lacks:")
            ),
            Decision::Blocked { missing } => format!(
                "BLOCKED: {tag} (build {build}) latched by BLOCKED-{}-{build}.md — delete it to \
                 retry{}",
                self.program.name,
                lacks(missing, "the staging release lacks:")
            ),
            Decision::Pack { missing } => format!(
                "PACK: {tag} (build {build}) > pin {} — at {} for: {}{}",
                self.pin.unwrap_or(0),
                self.release
                    .as_ref()
                    .map_or("?", |r| &r.dev_commit[..r.dev_commit.len().min(12)]),
                super::state::names(self.program.darwin_triples()),
                lacks(missing, "re-packing: the staging release lacks")
            ),
        }
    }
}

/// What one run did.
#[derive(Clone, Debug, Default)]
pub struct Outcome {
    /// Another run held the lock: this one did nothing.
    pub covered: bool,
    /// The verified head it decided over.
    pub head: Option<u64>,
    /// The decision table.
    pub rows: Vec<Row>,
    /// The gaps to main.
    pub gaps: Vec<MainGap>,
    /// Programs packed this run.
    pub packed: Vec<String>,
    /// Programs staged but held by their group.
    pub held: Vec<String>,
    /// Programs pinned by the index this run published.
    pub pinned: Vec<String>,
    /// The index this run published.
    pub published: Option<u64>,
}

/// One run of the Apple lane.
///
/// # Errors
///
/// A refusal for a downgrade, an unverifiable head, a stalled run, a failed index publish
/// or a run whose every candidate failed to pack; a could-not-run when the machine lacks
/// what a signing run needs.
pub fn run(ctx: &Ctx, opts: &Options, deps: &Deps<'_>, tx: &Transcript) -> Result<Outcome> {
    let state = LaneState::of(LaneKind::Alab, &ctx.state_dir);
    state.ensure()?;
    let _guard = match state.try_lock()? {
        Lock::Held(g) => g,
        Lock::Busy => {
            if let Some(started) = state.stalled_since(ctx.now_unix) {
                let msg = format!(
                    "the run holding {} started {}, more than {}h ago — a stalled run (a hung \
                     build? a prompt?); find it with `pgrep -fl 'atpkg-publish lane run alab'` \
                     and kill it",
                    state.lock.display(),
                    aterm_types::rfc3339::format_rfc3339(u64::try_from(started).unwrap_or(0)),
                    super::state::STALLED_AFTER_SECS / 3600
                );
                notice(
                    ctx,
                    opts,
                    deps,
                    &state,
                    "a stalled run has held the lane lock for more than 12h — see alab.log",
                );
                return Err(Error::refused(msg));
            }
            tx.say(format!(
                "another run holds {} — it covers this one; exiting 0",
                file_name(&state.lock)
            ));
            return Ok(Outcome {
                covered: true,
                ..Outcome::default()
            });
        }
    };
    let result = locked(ctx, opts, deps, &state, tx);
    if !opts.dry_run {
        state.clear_started();
    }
    if let Err(e) = &result {
        notice(ctx, opts, deps, &state, &format!("FAILED: {e}"));
    }
    result
}

fn notice(ctx: &Ctx, opts: &Options, deps: &Deps<'_>, state: &LaneState, message: &str) {
    report::notify_once(
        deps.notifier,
        TITLE,
        &state.dir,
        ctx.now_unix,
        opts.dry_run,
        message,
    );
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into(),
    )
}

#[allow(clippy::too_many_lines)]
fn locked(
    ctx: &Ctx,
    opts: &Options,
    deps: &Deps<'_>,
    state: &LaneState,
    tx: &Transcript,
) -> Result<Outcome> {
    if !opts.dry_run {
        state.mark_started(ctx.now_unix);
    }
    tx.raw(format!(
        "=== {ME} {} pid {}{} ===",
        aterm_types::rfc3339::format_rfc3339(u64::try_from(ctx.now_unix).unwrap_or(0)),
        std::process::id(),
        if opts.dry_run { " (DRY_RUN)" } else { "" }
    ));
    if !opts.dry_run {
        preflight(ctx, deps)?;
        state.prune(ctx.now_unix);
    }
    let work = state.dir.join("work");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work)
        .map_err(|e| Error::io(format!("mkdir {}", work.display()), &e))?;

    // --- the channel head the fleet sees, verified ---
    let slug = ctx.index_slug();
    let (n, head) = match quad::fetch_head(ctx, &slug, &work.join("head")) {
        Ok(h) => h,
        Err(Error::CannotRun(why)) => {
            let fails = state.bump_head_failures(opts.dry_run);
            tx.say(format!(
                "could not read an atpkg-index-N release on {slug} ({why}; consecutive \
                 failures: {fails}) — nothing to decide; exiting 0"
            ));
            if fails >= HEAD_FAILURES_NOTIFY {
                notice(
                    ctx,
                    opts,
                    deps,
                    state,
                    &format!("the channel head on {slug} has been unreadable for {fails} runs"),
                );
            }
            return Ok(Outcome::default());
        }
        Err(e) => {
            return Err(Error::refused(format!(
                "the channel head on {slug} is not a baseline this lane will build on — {e}"
            )));
        }
    };
    if !opts.dry_run {
        state.clear_head_failures();
    }
    state.check_index(n)?;
    tx.say(format!(
        "channel index head: atpkg-index-{n} (last processed: {})",
        state.index_seen()?
    ));
    let channel = head.index.channels.first().cloned().ok_or_else(|| {
        Error::refused(format!(
            "index-{n} carries no [[channels]] block — no pins to track"
        ))
    })?;
    let pins = crate::pins::for_targets(&channel, TARGETS)?;
    state.check_pins(TARGETS, &pins, n, tx)?;
    let groups = super::hold::groups_of(&head.index, pins.keys().map(String::as_str))?;
    if !opts.dry_run {
        // Every pack and the index are signed by this key: one the head's roster does not
        // list as live signs documents no client accepts.
        crate::identity::signer_for(ctx, &head.roster)?;
    }
    let horizon = horizon_note(ctx, &head)?;
    if let Some(h) = &horizon {
        tx.warn(h);
        notice(ctx, opts, deps, state, h);
    }

    // --- per program: the newest signed release against the pin ---
    let mut clones = Clones::new(ctx, state, opts.dry_run);
    let gaps: Vec<MainGap> = programs::PROGRAMS
        .iter()
        .filter(|p| matches!(p.shape, Shape::Bundle(b) if b.main_gap))
        .filter_map(|p| {
            let pin = *pins.get(p.name)?;
            let clone = clones.of(p);
            Some(gap::read(
                p.name,
                pin,
                clone.as_deref().map_err(String::clone),
                (deps.pin_commit)(ctx, p.name, pin),
                opts.dry_run,
            ))
        })
        .collect();
    let mut rows = Vec::new();
    let mut required_of: BTreeMap<&str, BTreeSet<Triple>> = BTreeMap::new();
    for def in programs::PROGRAMS {
        let facts = gather(ctx, opts, state, &head, &pins, &work, &mut clones, def, tx);
        if let Staging::Known { required, .. } = &facts.staging {
            required_of.insert(def.name, required.clone());
        }
        let decision = decide(&facts);
        rows.push(Row {
            program: def,
            pin: facts.pin,
            release: facts.release,
            decision,
        });
    }
    let staging = ctx.accounts.staging.clone();
    say_table(tx, n, &channel.name, &head, &rows, &gaps, &staging);
    for g in &gaps {
        if g.is_unknown() {
            tx.warn(g.line());
        } else {
            tx.say(g.line());
        }
    }
    let human: Vec<&Row> = rows
        .iter()
        .filter(|r| matches!(r.decision, Decision::HumanLane { .. }))
        .collect();
    for r in &human {
        tx.say(format!(
            "HUMAN-LANE {} — on a rostered machine:",
            r.program.name
        ));
        for line in bundle_command(r) {
            tx.raw(format!("    {line}"));
        }
    }

    let mut not_ready: BTreeSet<&str> = rows
        .iter()
        .filter(|r| r.pin.is_some() && !r.decision.ready())
        .map(|r| r.program.name)
        .collect();
    let candidates: Vec<&Row> = rows
        .iter()
        .filter(|r| matches!(r.decision, Decision::Pack { .. }))
        .collect();
    let mut pending: Vec<&str> = rows
        .iter()
        .filter(|r| r.decision == Decision::Index)
        .map(|r| r.program.name)
        .collect();
    let mut outcome = Outcome {
        head: Some(n),
        gaps: gaps.clone(),
        ..Outcome::default()
    };

    if opts.dry_run {
        let holds = holds_of(&groups, &not_ready);
        let (pin_now, held) = partition(
            pending
                .iter()
                .copied()
                .chain(candidates.iter().map(|r| r.program.name)),
            &groups,
            &holds,
        );
        tx.say(format!(
            "DRY_RUN — would pack: {}; would publish index {} pinning: {}; packed but HELD: {}; \
             human lane: {}; no clone/build/sign/upload/state change",
            list(candidates.iter().map(|r| r.program.name)),
            n + 1,
            list(pin_now.iter().copied()),
            list(held.iter().copied()),
            list(human.iter().map(|r| r.program.name)),
        ));
        outcome.rows = rows.clone();
        return Ok(outcome);
    }

    if candidates.is_empty() && pending.is_empty() {
        state.record(n, TARGETS, &pins)?;
        write_sync(ctx, state, None, &[], &human, &gaps, horizon.as_deref())?;
        notify_gaps(ctx, opts, deps, state, &gaps);
        if !human.is_empty() {
            notice(
                ctx,
                opts,
                deps,
                state,
                &format!(
                    "human lane: {} have newer signed releases than their pins (see ALAB-SYNC.md)",
                    list(human.iter().map(|r| r.program.name))
                ),
            );
        }
        tx.say(format!(
            "every tracked program is at its newest signed release (or waits on the human lane) \
             — clean no-op{}",
            owed_note(&gaps)
        ));
        outcome.rows = rows.clone();
        return Ok(outcome);
    }

    // --- pack each candidate at its tag, per target, uploaded to the staging owner ---
    for r in &candidates {
        let required = required_of.get(r.program.name).cloned().unwrap_or_default();
        match pack(ctx, deps, state, &mut clones, r, &required, tx) {
            Ok(()) => {
                outcome.packed.push(r.program.name.to_string());
                pending.push(r.program.name);
            }
            Err(headline) => {
                let build = r.build().unwrap_or(0);
                fail_pack(ctx, opts, deps, state, r.program.name, build, &headline, tx);
                not_ready.insert(r.program.name);
            }
        }
    }

    // --- the hold partition: what this index may pin ---
    let holds = holds_of(&groups, &not_ready);
    let (pin_now, held) = partition(pending.iter().copied(), &groups, &holds);
    for p in &held {
        let build = rows
            .iter()
            .find(|r| r.program.name == *p)
            .and_then(Row::build)
            .unwrap_or(0);
        let group = groups.get(*p).and_then(Option::as_deref).unwrap_or("");
        tx.warn(format!(
            "{p} build {build} is packed on staging but HOLD (group {group} moves all-or-nothing; \
             a member is not ready: {})",
            list(
                holds
                    .holding(group, None)
                    .unwrap_or_default()
                    .iter()
                    .map(String::as_str)
            )
        ));
    }
    outcome.held = held.iter().map(|p| (*p).to_string()).collect();
    if pin_now.is_empty() {
        state.record(n, TARGETS, &pins)?;
        write_sync(ctx, state, None, &[], &human, &gaps, horizon.as_deref())?;
        notify_gaps(ctx, opts, deps, state, &gaps);
        if !held.is_empty() {
            notice(
                ctx,
                opts,
                deps,
                state,
                &format!(
                    "packed but HELD by a coherence group: {} — the human lane ({}) unblocks them \
                     (see ALAB-SYNC.md)",
                    list(held.iter().copied()),
                    list(human.iter().map(|r| r.program.name))
                ),
            );
        }
        outcome.rows = rows.clone();
        if outcome.packed.is_empty() && held.is_empty() {
            return Err(Error::refused(format!(
                "every candidate failed to pack — see the attempts/BLOCKED markers under {}",
                ctx.shown(&state.dir)
            )));
        }
        tx.say(format!(
            "done — packed this run: {}; held: {}; no index published (the group waits on: {})",
            list(outcome.packed.iter().map(String::as_str)),
            list(held.iter().copied()),
            if human.is_empty() {
                "the blocked members".to_string()
            } else {
                list(human.iter().map(|r| r.program.name))
            }
        ));
        return Ok(outcome);
    }

    // --- publish index N+1 over the verified head ---
    let mut delta = Delta::default();
    let mut moved = pins.clone();
    for p in &pin_now {
        if let Some(b) = rows
            .iter()
            .find(|r| r.program.name == *p)
            .and_then(Row::build)
        {
            delta.pins.insert((*p).to_string(), b);
            moved.insert((*p).to_string(), b);
        }
    }
    if !not_ready.is_empty() || !held.is_empty() {
        tx.warn(format!(
            "publishing with pins this run knowingly carries unmoved: held={} human-lane={} \
             not-ready={}",
            list(held.iter().copied()),
            list(human.iter().map(|r| r.program.name)),
            list(not_ready.iter().copied())
        ));
    }
    tx.say(format!(
        "publishing atpkg-index-{} (baseline index-{n}) to {slug}",
        n + 1
    ));
    let published = deps
        .indexer
        .publish(
            ctx,
            &IndexRequest {
                baseline: Baseline::Dir(head.dir.clone()),
                delta,
                out: state.dir.join("index-out"),
                upload: true,
            },
        )
        .map_err(|e| {
            Error::refused(format!(
                "index {} did not publish ({e}) — the packed builds are on staging and the next \
                 run resumes at the index step",
                n + 1
            ))
        })?;
    state.record(published.index_build, TARGETS, &moved)?;
    let pinned: Vec<(&str, String, Build)> = pin_now
        .iter()
        .filter_map(|p| {
            let r = rows.iter().find(|r| r.program.name == *p)?;
            let rel = r.release.as_ref().ok()?;
            Some((*p, rel.tag.clone(), rel.build))
        })
        .collect();
    write_sync(
        ctx,
        state,
        Some(published.index_build),
        &pinned,
        &human,
        &gaps,
        horizon.as_deref(),
    )?;
    notify_gaps(ctx, opts, deps, state, &gaps);
    let summary = format!(
        "published atpkg-index-{}:{}",
        published.index_build,
        pinned
            .iter()
            .map(|(p, t, b)| format!(" {p} {t} (build {b})"))
            .collect::<String>()
    );
    tx.say(&summary);
    notice(ctx, opts, deps, state, &summary);
    tx.say(format!(
        "done — packed this run: {}; held: {}; human lane: {}; index {} live on {slug}",
        list(outcome.packed.iter().map(String::as_str)),
        list(held.iter().copied()),
        list(human.iter().map(|r| r.program.name)),
        published.index_build
    ));
    outcome.pinned = pin_now.iter().map(|p| (*p).to_string()).collect();
    outcome.published = Some(published.index_build);
    outcome.rows = rows.clone();
    Ok(outcome)
}

/// A signing run needs the machine key, its record and both token files (the lanes never
/// use gh's own login), and a packer that can pack ([`Packer::can_pack`]: the zstd CLI
/// every pack is compressed by), so a missing tool refuses the run before any build
/// rather than failing every pack until each program latches.
fn preflight(ctx: &Ctx, deps: &Deps<'_>) -> Result<()> {
    if !ctx.identity.key.is_file() {
        return Err(Error::cannot_run(format!(
            "no machine key at {} — this lane SIGNS; provision this Mac (`atpkg-keys join --id \
             <id>`) or run it on a rostered one",
            ctx.shown(&ctx.identity.key)
        )));
    }
    if !ctx.identity.record.is_file() {
        return Err(Error::cannot_run(format!(
            "no machine record at {} (written by `atpkg-keys setup`/`join`)",
            ctx.shown(&ctx.identity.record)
        )));
    }
    for (what, path) in [
        ("channel", &ctx.tokens.channel),
        ("staging", &ctx.tokens.staging),
    ] {
        if std::fs::metadata(path).map_or(true, |m| m.len() == 0) {
            return Err(Error::cannot_run(format!(
                "{what} token file missing/empty: {} — the unattended lane never uses gh's own \
                 login",
                ctx.shown(path)
            )));
        }
    }
    deps.packer
        .can_pack()
        .map_err(|why| Error::cannot_run(format!("this Mac cannot pack: {why}")))
}

fn horizon_note(ctx: &Ctx, head: &VerifiedQuad) -> Result<Option<String>> {
    let vu = aterm_types::rfc3339::parse_utc(&head.index.valid_until).ok_or_else(|| {
        Error::refused(format!(
            "the head's valid_until `{}` cannot be parsed",
            head.index.valid_until
        ))
    })?;
    let left = (vu - ctx.now_unix) / 86_400;
    Ok((left < HORIZON_WARN_DAYS).then(|| {
        format!(
            "index horizon {} is {left} day(s) away — publish with a later `--valid-until` \
             before clients refuse the index",
            head.index.valid_until
        )
    }))
}

/// The scratch clones of the program repos under `<state>/alab/progs/`, each fetched at
/// most once a run (a dry run reads what is there and fetches nothing).
struct Clones<'a> {
    ctx: &'a Ctx,
    dir: PathBuf,
    dry_run: bool,
    seen: BTreeMap<String, std::result::Result<PathBuf, String>>,
}

impl<'a> Clones<'a> {
    fn new(ctx: &'a Ctx, state: &LaneState, dry_run: bool) -> Self {
        Self {
            ctx,
            dir: state.dir.join("progs"),
            dry_run,
            seen: BTreeMap::new(),
        }
    }

    fn of(&mut self, def: &ProgramDef) -> std::result::Result<PathBuf, String> {
        if let Some(r) = self.seen.get(def.repo) {
            return r.clone();
        }
        let r = self.open(def);
        self.seen.insert(def.repo.to_string(), r.clone());
        r
    }

    fn open(&self, def: &ProgramDef) -> std::result::Result<PathBuf, String> {
        let clone = self.dir.join(def.repo);
        let slug = self.ctx.staging_slug(def.repo);
        if !clone.join(".git").exists() {
            if self.dry_run {
                return Err(format!(
                    "no scratch clone at {} yet (a real run clones {slug})",
                    self.ctx.shown(&clone)
                ));
            }
            std::fs::create_dir_all(&self.dir).map_err(|e| format!("mkdir: {e}"))?;
            self.ctx
                .hosts
                .staging
                .clone_repo(&slug, &clone)
                .map_err(|e| format!("clone of {slug} failed: {e}"))?;
        } else if !self.dry_run {
            self.ctx
                .hosts
                .staging
                .fetch_repo(&slug, &clone)
                .map_err(|e| format!("git fetch failed in {}: {e}", self.ctx.shown(&clone)))?;
        }
        Ok(clone)
    }
}

#[allow(clippy::too_many_arguments)]
fn gather(
    ctx: &Ctx,
    opts: &Options,
    state: &LaneState,
    head: &VerifiedQuad,
    pins: &BTreeMap<String, Build>,
    work: &Path,
    clones: &mut Clones<'_>,
    def: &'static ProgramDef,
    tx: &Transcript,
) -> Facts {
    let pin = pins.get(def.name).copied();
    let mut facts = Facts {
        program: def,
        pin,
        release: Err(ReleaseGap::NoTag(String::new())),
        staging: Staging::NotLooked,
        latched: false,
    };
    let Some(pin) = pin else {
        return facts;
    };
    facts.release = signed_release(ctx, opts, head, work, clones, def);
    match &facts.release {
        Err(ReleaseGap::NoTag(why)) => tx.warn(format!("{}: {why} — skipped this run", def.name)),
        Err(ReleaseGap::Unverified { tag, why }) => {
            tx.warn(format!("{}: {tag} is not built — {why}", def.name));
        }
        Err(ReleaseGap::Unresolved { tag, why }) => tx.warn(format!(
            "{}: {tag} cannot be resolved to a dev commit: {why}",
            def.name
        )),
        Ok(_) => {}
    }
    let Ok(rel) = &facts.release else {
        return facts;
    };
    facts.latched = state.blocked_file(def.name, rel.build).exists();
    if rel.build <= pin {
        return facts;
    }
    facts.staging = staging_of(ctx, head, work, def, pin, rel.build);
    if let Staging::Known {
        required,
        staged: Some(staged),
    } = &facts.staging
    {
        let missing: Vec<Triple> = required.difference(staged).copied().collect();
        if !missing.is_empty() {
            tx.warn(format!(
                "{}: {} on {} lacks target row(s): {} — a half-packed release (a leg failed \
                 after an earlier one uploaded) is never pinned",
                def.name,
                rows::tag(def.name, rel.build),
                ctx.accounts.staging,
                super::state::names(&missing)
            ));
        }
    }
    if let Staging::Unknown(why) = &facts.staging {
        tx.warn(format!(
            "{}: {why}; skipped this run (nothing packed, nothing pinned)",
            def.name
        ));
    }
    facts
}

/// The newest signed release of `def`'s repo on the public owner, verified and bound to a
/// dev commit in the scratch clone.
fn signed_release(
    ctx: &Ctx,
    opts: &Options,
    head: &VerifiedQuad,
    work: &Path,
    clones: &mut Clones<'_>,
    def: &ProgramDef,
) -> std::result::Result<SignedRelease, ReleaseGap> {
    let slug = ctx.public_slug(def.repo);
    let releases = ctx.hosts.channel.releases(&slug).map_err(|e| {
        ReleaseGap::NoTag(format!("no signed vX.Y.Z release readable on {slug} ({e})"))
    })?;
    let tag = newest_signed_tag(&releases)
        .ok_or_else(|| ReleaseGap::NoTag(format!("no signed vX.Y.Z release readable on {slug}")))?;
    let public_commit =
        signed_commit(ctx, head, work, def, &tag).map_err(|why| ReleaseGap::Unverified {
            tag: tag.clone(),
            why,
        })?;
    let unresolved = |why: String| ReleaseGap::Unresolved {
        tag: tag.clone(),
        why,
    };
    let clone = clones.of(def).map_err(unresolved)?;
    let dev_commit =
        resolve_dev(ctx, &opts.ledger, def.repo, &public_commit, &clone).map_err(unresolved)?;
    let build = crate::git::rev_count(&clone, &dev_commit).map_err(|e| ReleaseGap::Unresolved {
        tag: tag.clone(),
        why: e.to_string(),
    })?;
    Ok(SignedRelease {
        tag,
        public_commit,
        dev_commit,
        build,
    })
}

/// The numerically newest `vX.Y.Z` among published (non-draft, non-prerelease) releases
/// carrying `SHA256SUMS.sig`.
#[must_use]
pub fn newest_signed_tag(releases: &[Release]) -> Option<String> {
    releases
        .iter()
        .filter(|r| !r.draft && !r.prerelease)
        .filter(|r| r.assets.iter().any(|a| a == "SHA256SUMS.sig"))
        .filter_map(|r| Some((semver(&r.tag)?, r.tag.clone())))
        .max()
        .map(|(_, t)| t)
}

fn semver(tag: &str) -> Option<(u64, u64, u64)> {
    let mut parts = tag.strip_prefix('v')?.split('.');
    let v = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(v)
}

/// The most a release's `SHA256SUMS` (or its `.sig`) may be: it lists every file the
/// release ships, so it outgrows a manifest (trust v0.8.0's is 8.7 MB); the cap only keeps
/// an attacker-served asset from filling the disk.
pub const SUMS_CAP: u64 = 64 << 20;

/// The public commit the release's signed `SHA256SUMS` names. The pair is read as a client
/// reads it (the anonymous download host) and verified under the channel's roster and the
/// compiled paper master; its header must name this repo and tag, so a SUMS re-served from
/// another tag or repo is refused.
fn signed_commit(
    ctx: &Ctx,
    head: &VerifiedQuad,
    work: &Path,
    def: &ProgramDef,
    tag: &str,
) -> std::result::Result<String, String> {
    let slug = ctx.public_slug(def.repo);
    let dir = work.join("sums").join(def.repo).join(tag);
    let mut bytes = Vec::new();
    for f in ["SHA256SUMS", "SHA256SUMS.sig"] {
        match ctx
            .hosts
            .channel
            .fetch_public(&slug, tag, f, &dir.join(f), SUMS_CAP)
        {
            Probe::Present => {}
            Probe::Absent => return Err(format!("{f} of {slug} {tag} is not published")),
            Probe::Unknown(why) => return Err(format!("{f} of {slug} {tag}: {why}")),
        }
        bytes.push(std::fs::read(dir.join(f)).map_err(|e| format!("read {f}: {e}"))?);
    }
    let [_, _, roster_file, roster_sig_file] = quad::QUAD_FILES;
    let roster = std::fs::read(head.file(roster_file)).map_err(|e| e.to_string())?;
    let roster_sig = std::fs::read(head.file(roster_sig_file)).map_err(|e| e.to_string())?;
    let anchor: Vec<&str> = ctx.anchor.iter().map(String::as_str).collect();
    atpkg_keys::verify_signed(
        &anchor,
        roster,
        &roster_sig,
        &bytes[0],
        &bytes[1],
        ctx.now_unix,
    )
    .map_err(|e| format!("SHA256SUMS.sig does not verify under the channel's roster: {e:?}"))?;
    let text = String::from_utf8_lossy(&bytes[0]);
    let field = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key))
            .map(|v| v.trim().to_string())
    };
    let (got_repo, got_tag) = (
        field("# repo: ").unwrap_or_default(),
        field("# tag: ").unwrap_or_default(),
    );
    if got_repo != slug.to_string() || got_tag != tag {
        return Err(format!(
            "the signed SHA256SUMS names {got_repo} {got_tag}, not {slug} {tag}"
        ));
    }
    field("# commit: ")
        .filter(|c| c.len() == 40 && c.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| "the signed SHA256SUMS names no commit".to_string())
}

/// The dev commit the engine ledger maps to `public_commit`, present in `clone`.
fn resolve_dev(
    ctx: &Ctx,
    ledger: &Path,
    repo: &str,
    public_commit: &str,
    clone: &Path,
) -> std::result::Result<String, String> {
    let short = &public_commit[..public_commit.len().min(12)];
    let text = std::fs::read_to_string(ledger).map_err(|_| {
        format!(
            "no engine ledger at {} to map the signed commit {short} to a dev commit",
            ctx.shown(ledger)
        )
    })?;
    let value: aterm_json::Value = aterm_json::from_str(&text)
        .map_err(|e| format!("{} does not parse: {e}", ctx.shown(ledger)))?;
    let src = value
        .get("mappings")
        .and_then(|m| m.get(repo))
        .and_then(aterm_json::Value::as_object)
        .and_then(|rows| {
            rows.iter().find_map(|(dev, row)| {
                (row.get("release_rev").and_then(aterm_json::Value::as_str) == Some(public_commit))
                    .then(|| dev.clone())
            })
        })
        .ok_or_else(|| {
            format!(
                "{} has no {repo} row whose release_rev is the signed commit {short} — `git -C \
                 ~/publication pull --ff-only`, or the promote ran elsewhere",
                ctx.shown(ledger)
            )
        })?;
    // The key reaches git as a revision: only a full sha, never a ref or an option.
    if src.len() != 40 || !src.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!(
            "{} maps the signed commit {short} to `{src}`, which is not a 40-hex dev commit",
            ctx.shown(ledger)
        ));
    }
    crate::git::run(
        clone,
        &["rev-parse", "-q", "--verify", &format!("{src}^{{commit}}")],
    )
    .ok()
    .filter(|s| !s.is_empty())
    .ok_or_else(|| {
        format!(
            "the ledger maps the signed commit to dev commit {}, which the clone does not \
                 have (fetch failed, or a rewritten history)",
            &src[..src.len().min(12)]
        )
    })
}

/// What staging holds for `def` `build`, and what that build must carry.
fn staging_of(
    ctx: &Ctx,
    head: &VerifiedQuad,
    work: &Path,
    def: &ProgramDef,
    pin: Build,
    build: Build,
) -> Staging {
    let slug = ctx.staging_slug(def.repo);
    let host = &ctx.hosts.staging;
    let mut required: BTreeSet<Triple> = def.darwin_triples().iter().copied().collect();
    match rows::fetch(host, &slug, def.name, pin, &work.join("pinned"), head) {
        Fetched::Present { manifest, .. } => {
            required.extend(
                rows::targets(&manifest)
                    .into_iter()
                    .filter(|t| t.os() == Os::Darwin),
            );
        }
        Fetched::Absent(_) => {}
        Fetched::Unreachable(why) => {
            return Staging::Unknown(format!(
                "{} unreachable — the pinned manifest {} could not be read ({why})",
                ctx.accounts.staging,
                rows::manifest_name(def.name, pin)
            ));
        }
        Fetched::Invalid(why) => {
            return Staging::Unknown(format!(
                "the pinned manifest {} does not verify ({why}) — the rows a new build must \
                 carry are unknown",
                rows::manifest_name(def.name, pin)
            ));
        }
    }
    match rows::fetch(host, &slug, def.name, build, &work.join("staged"), head) {
        Fetched::Present { manifest, .. } => Staging::Known {
            required,
            staged: Some(rows::targets(&manifest)),
        },
        Fetched::Absent(_) => Staging::Known {
            required,
            staged: None,
        },
        Fetched::Unreachable(why) => Staging::Unknown(format!(
            "{} unreachable — {} could not be looked up ({why})",
            ctx.accounts.staging,
            rows::tag(def.name, build)
        )),
        Fetched::Invalid(why) => Staging::Unknown(format!(
            "{} on {} does not verify ({why}) — never pinned, and never re-packed over",
            rows::tag(def.name, build),
            ctx.accounts.staging
        )),
    }
}

fn say_table(
    tx: &Transcript,
    n: u64,
    channel: &str,
    head: &VerifiedQuad,
    rows: &[Row],
    gaps: &[MainGap],
    staging: &str,
) {
    tx.say(format!(
        "decision table for atpkg-index-{n} (channel '{channel}', valid_until {}, signed \
         releases on the public owner):",
        head.index.valid_until
    ));
    let mut table = vec![vec![
        "PROGRAM".to_string(),
        "PINNED".into(),
        "NEWEST-TAG".into(),
        "TAG-BUILD".into(),
        "DECISION".into(),
    ]];
    for r in rows {
        let gap = gaps.iter().find(|g| g.program == r.program.name);
        table.push(vec![
            r.program.name.to_string(),
            r.pin.map_or_else(|| "—".into(), |p| p.to_string()),
            r.tag().unwrap_or("?").to_string(),
            r.build().map_or_else(|| "?".into(), |b| b.to_string()),
            r.render(n, staging, gap),
        ]);
    }
    for line in report::columns(&table) {
        tx.raw(line);
    }
}

/// Pack one candidate for every Apple target it ships, then hold the result to the row
/// gate. `Err` is the failure's headline, counted toward the latch by the caller.
fn pack(
    ctx: &Ctx,
    deps: &Deps<'_>,
    state: &LaneState,
    clones: &mut Clones<'_>,
    row: &Row,
    required: &BTreeSet<Triple>,
    tx: &Transcript,
) -> std::result::Result<(), String> {
    let def = row.program;
    let rel = row
        .release
        .as_ref()
        .map_err(|_| "no resolved release to pack".to_string())?;
    let (name, build) = (def.name, rel.build);
    let short = &rel.dev_commit[..rel.dev_commit.len().min(12)];
    let clone = clones.of(def)?;
    tx.say(format!(
        "[{name} {}] checking out {short} in {}",
        rel.tag,
        ctx.shown(&clone)
    ));
    crate::git::run(
        &clone,
        &["checkout", "--force", "--detach", &rel.dev_commit],
    )
    .map_err(|e| format!("checkout of {short} failed: {e}"))?;
    let have = crate::git::rev_count(&clone, "HEAD").map_err(|e| e.to_string())?;
    if have != build {
        return Err(format!(
            "the clone at {short} counts {have} commits, not {build} — the build number would \
             not name this commit"
        ));
    }
    let out = state.dir.join("out").join(name);
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out).map_err(|e| format!("mkdir {}: {e}", out.display()))?;
    let mut base: Option<PathBuf> = None;
    let mut last: Option<PathBuf> = None;
    for triple in pack_order(def.darwin_triples()) {
        tx.say(format!(
            "[{name} {}] packing {triple} (build {build})",
            rel.tag
        ));
        let outcome = deps
            .packer
            .pack(
                ctx,
                &PackRequest {
                    program: name.to_string(),
                    checkout: clone.clone(),
                    triple,
                    build: Some(build),
                    merge_from: base.clone(),
                    toolchain: None,
                    out: out.join(triple.as_str()),
                    signing: Signing::MachineKey,
                    upload: true,
                    env: Vec::new(),
                },
            )
            .map_err(|e| format!("the pack failed for {triple}: {e}"))?;
        if outcome.build != build {
            return Err(format!(
                "the {triple} pack reports build {}, not {build}",
                outcome.build
            ));
        }
        // The next leg's base is a copy beside the stage: a pack owns (and may clear) its
        // own output, and a merge base inside it could vanish under the next leg.
        let copy = out.join(format!("merge-base-{name}-{build}.toml"));
        std::fs::copy(&outcome.manifest, &copy)
            .map_err(|e| format!("copy {}: {e}", outcome.manifest.display()))?;
        base = Some(copy);
        last = Some(outcome.manifest);
    }
    let manifest = last.ok_or_else(|| "the program carries no Apple target to pack".to_string())?;
    let carried = rows::local(&manifest)
        .map(|m| rows::targets(&m))
        .map_err(|e| e.to_string())?;
    let lost: Vec<Triple> = required.difference(&carried).copied().collect();
    if !lost.is_empty() {
        return Err(format!(
            "the packed manifest lacks target row(s): {} — a dropped platform is never pinned \
             (release {} stays on staging, unpinned)",
            super::state::names(&lost),
            rows::tag(name, build)
        ));
    }
    state.clear_attempts(name, build);
    tx.say(format!(
        "[{name} {}] staged {} on {} ({})",
        rel.tag,
        rows::tag(name, build),
        ctx.staging_slug(def.repo),
        super::state::names(def.darwin_triples())
    ));
    Ok(())
}

/// aarch64 first (the fresh manifest), every other target merged onto it.
fn pack_order(triples: &[Triple]) -> Vec<Triple> {
    let mut v: Vec<Triple> = triples.to_vec();
    v.sort_by_key(|t| *t != AARCH64_APPLE_DARWIN);
    v
}

#[allow(clippy::too_many_arguments)]
fn fail_pack(
    ctx: &Ctx,
    opts: &Options,
    deps: &Deps<'_>,
    state: &LaneState,
    program: &str,
    build: Build,
    headline: &str,
    tx: &Transcript,
) {
    let n = state.bump_attempts(program, build);
    if n < opts.block_after {
        tx.warn(format!(
            "{program} build {build}: {headline} (attempt {n} of {} — retrying next run)",
            opts.block_after
        ));
        notice(
            ctx,
            opts,
            deps,
            state,
            &format!(
                "{program} build {build}: pack failed (attempt {n} of {}) — {headline}",
                opts.block_after
            ),
        );
        return;
    }
    let marker = state.blocked_file(program, build);
    let text = format!(
        "# auto-alab: {program} build {build} BLOCKED after {n} attempts\n\n{headline}\n\n- this \
         file is the latch: the lane skips {program} build {build} without rebuilding while it \
         exists.\n  Delete it (and attempts-{program}-{build}) to retry; a newer signed release \
         retries by itself.\n"
    );
    let _ = report::write_atomic(&marker, &text);
    tx.warn(format!(
        "{program} build {build} BLOCKED after {n} attempts — {headline} (marker: {})",
        ctx.shown(&marker)
    ));
    notice(
        ctx,
        opts,
        deps,
        state,
        &format!("{program} build {build} blocked after {n} attempts: {headline}"),
    );
}

/// The groups held this run: a pinned member of a group not ready holds it (a member
/// this lane does not track is never advanced, and never holds).
fn holds_of(groups: &BTreeMap<String, Option<String>>, not_ready: &BTreeSet<&str>) -> Holds {
    super::hold::of_members(groups.iter().filter_map(|(p, g)| {
        Some((
            g.as_deref()?,
            p.as_str(),
            None,
            !not_ready.contains(p.as_str()),
        ))
    }))
}

fn partition<'a>(
    programs: impl Iterator<Item = &'a str>,
    groups: &BTreeMap<String, Option<String>>,
    holds: &Holds,
) -> (Vec<&'a str>, Vec<&'a str>) {
    programs.partition(|p| {
        groups
            .get(*p)
            .and_then(Option::as_deref)
            .is_none_or(|g| !holds.is_held(g, None))
    })
}

fn list<'a>(items: impl IntoIterator<Item = &'a str>) -> String {
    let v: Vec<&str> = items.into_iter().collect();
    if v.is_empty() {
        "<none>".into()
    } else {
        v.join(" ")
    }
}

fn owed_note(gaps: &[MainGap]) -> String {
    let owed: Vec<String> = gaps
        .iter()
        .filter(|g| g.owed().is_some())
        .map(|g| format!("{} ({} above)", g.program, g.label()))
        .collect();
    if owed.is_empty() {
        String::new()
    } else {
        format!(
            "; a cut is owed: {} — the cut is in ALAB-SYNC.md",
            owed.join(", ")
        )
    }
}

fn notify_gaps(ctx: &Ctx, opts: &Options, deps: &Deps<'_>, state: &LaneState, gaps: &[MainGap]) {
    for g in gaps {
        if let Some(n) = g.notice() {
            notice(ctx, opts, deps, state, &n);
        }
    }
}

/// The human lane's pack of a bundle, as a person runs it on a rostered machine: the
/// `rustc` group's bundle is the group publish from one seal; any other bundle one
/// `pack-bundle` leg per required target, aarch64 first and every other leg merged onto a
/// copy of it, so the release it leaves is never half-packed.
#[must_use]
pub fn bundle_command(row: &Row) -> Vec<String> {
    let def = row.program;
    let (Ok(rel), Shape::Bundle(b)) = (&row.release, def.shape) else {
        return Vec::new();
    };
    let (name, build) = (def.name, rel.build);
    let mut out = vec![format!(
        "git -C ~/{} fetch --tags origin && git -C ~/{} checkout --detach {}   # {} = build {build}",
        def.repo, def.repo, rel.dev_commit, rel.tag
    )];
    if def.group == Some(programs::RUSTC_GROUP) {
        out.push(
            "# build + seal the toolchain at that commit, and stage the trust-wp drivers into a \
             copy of the seal (docs/RUNBOOK-atpkg-republish.md); the group publish packs it with \
             its siblings, then:"
                .into(),
        );
        out.push(format!(
            "atpkg-publish publish-group {} --seal <the staged copy of the seal> --dry-run",
            programs::RUSTC_GROUP
        ));
        out.push(format!(
            "atpkg-publish publish-group {} --seal <the staged copy of the seal> --upload",
            programs::RUSTC_GROUP
        ));
    } else {
        let required = match &row.decision {
            Decision::HumanLane { required, .. } => required.iter().copied().collect(),
            _ => b.darwin.to_vec(),
        };
        let dir = format!("<out dir>/pack-{name}-{build}");
        let manifest = format!("{dir}/{}", rows::manifest_name(name, build));
        let merge_base = format!("{dir}/merge-base-{name}-{build}.toml");
        out.push(
            "# build + stage the sysroot of each target as docs/RUNBOOK-atpkg-republish.md \
             describes, then:"
                .into(),
        );
        for t in pack_order(&required) {
            let mut leg = format!(
                "atpkg-publish pack-bundle {name} --sysroot <staged {t} sysroot> --src-repo ~/{} \
                 --build {build} --triple {t}",
                def.repo
            );
            if t != AARCH64_APPLE_DARWIN {
                out.push(format!("cp -f {manifest} {merge_base}"));
                leg.push_str(&format!(" --merge-from {merge_base}"));
            }
            if let Hello::TrustSysroot { compiler } = b.hello {
                leg.push_str(&format!(
                    " --hello-sysroot <the Trust sysroot this leg's {compiler} was built \
                     against: its -vV commit, never a rustup toolchain>"
                ));
            }
            leg.push_str(&format!(" --out {dir} --upload"));
            out.push(leg);
        }
    }
    out.push(format!(
        "# then re-run this lane (or `launchctl kickstart` it): the staging release {} is what \
         it waits for",
        rows::tag(name, build)
    ));
    out
}

fn write_sync(
    ctx: &Ctx,
    state: &LaneState,
    published: Option<u64>,
    pinned: &[(&str, String, Build)],
    human: &[&Row],
    gaps: &[MainGap],
    horizon: Option<&str>,
) -> Result<()> {
    let mut s = format!(
        "# ALAB-SYNC — the ALab pack tracker's state after {}\n",
        aterm_types::rfc3339::format_rfc3339(u64::try_from(ctx.now_unix).unwrap_or(0))
    );
    if let Some(n) = published {
        s.push_str(&format!(
            "\nPublished atpkg-index-{n}:{}\n",
            pinned
                .iter()
                .map(|(p, t, b)| format!(" {p} {t} (build {b})"))
                .collect::<String>()
        ));
    }
    if !human.is_empty() {
        s.push_str(
            "\nHUMAN LANE — sysroot bundles this lane never builds (the group they anchor is \
             HELD until they land):\n",
        );
        for r in human {
            if let Ok(rel) = &r.release {
                s.push_str(&format!(
                    "\n## {}: {} = build {} (pin {})\n\n```sh\n",
                    r.program.name,
                    rel.tag,
                    rel.build,
                    r.pin.unwrap_or(0)
                ));
                for line in bundle_command(r) {
                    s.push_str(&line);
                    s.push('\n');
                }
                s.push_str("```\n");
            }
        }
    }
    for g in gaps {
        let (Some(_), Some(sha), Some(mb)) = (g.owed(), g.main_sha(), g.main_build()) else {
            continue;
        };
        let days = g.days().map(|d| format!(", {d} days")).unwrap_or_default();
        s.push_str(&format!(
            "\n{} CUT OWED — {} main is build {mb} ({}); the published build {} is {}{days}:\n\n{}\n",
            g.program.to_uppercase(),
            g.program,
            &sha[..sha.len().min(10)],
            g.pin,
            g.words(),
            g.line()
        ));
        if g.program == "trust" {
            s.push_str(
                "\nThe cut, as trust's docs/CUTTING-A-TOOLCHAIN.md §3.9 runs it; the candidate \
                 gate (D27) refuses a compiler that proves any library less than the published \
                 seal, or gives one no result in its wall budget, and then NOTHING below it \
                 runs:\n\n```sh\n",
            );
            for line in gap::trust_cut_commands(sha) {
                s.push_str(&line);
                s.push('\n');
            }
            s.push_str("```\n");
        }
    }
    if let Some(h) = horizon {
        s.push_str(&format!("\nHORIZON: {h}\n"));
    }
    report::write_atomic(&state.dir.join("ALAB-SYNC.md"), &s)
}

/// The pin's commit and date as its installed compiler reports them:
/// `<prefix>/store/<program>/<build>/bin/trustc -vV`.
#[must_use]
pub fn installed_pin_commit(ctx: &Ctx, program: &str, build: Build) -> Option<PinCommit> {
    let trustc = ctx
        .pkg_prefix
        .join("store")
        .join(program)
        .join(build.to_string())
        .join("bin/trustc");
    let out = aterm_uds::spawnfd::command(&trustc)
        .arg("-vV")
        .output()
        .ok()?;
    parse_vv(&String::from_utf8_lossy(&out.stdout))
}

/// A compiler's `-vV` → its commit (the first 9 hex) and date.
#[must_use]
pub fn parse_vv(text: &str) -> Option<PinCommit> {
    let field = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key))
            .map(str::trim)
    };
    let commit = field("commit-hash: ")?;
    if commit.len() < 9 || !commit.bytes().take(9).all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let date = field("commit-date: ")
        .filter(|d| d.len() >= 10)
        .map(|d| d[..10].to_string());
    Some(PinCommit {
        commit: commit[..9].to_string(),
        date,
    })
}
