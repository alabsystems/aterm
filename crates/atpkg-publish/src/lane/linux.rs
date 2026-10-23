// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Linux build and publication lane ([`Builder`]).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use atpkg::PkgManifest;

use super::LaneKind;
use super::hold::Holds;
use super::report::{self, Transcript};
use super::rows::{self, Fetched};
use super::state::{LaneState, Lock, names};
use crate::api::{Baseline, Build, Delta, IndexRequest, Indexer, PackRequest, Packer, Signing};
use crate::artifact::lineage_switch::{self, Switch};
use crate::artifact::{pkgdoc, tarwrite, upload};
use crate::ctx::Ctx;
use crate::error::{Error, Result};
use crate::host::{Replace, Slug};
use crate::programs::{self, ProgramDef, Shape};
use crate::quad::{self, VerifiedQuad};
use crate::triple::{Os, Triple};

/// The transcript's prefix.
pub const ME: &str = "linux-auto-atpkg";

/// How one run is told to behave.
#[derive(Clone, Debug)]
pub struct Options {
    /// Decide and print; change nothing.
    pub dry_run: bool,
    /// The target this builder runs on.
    pub host: Triple,
    /// The Linux targets this lane serves.
    pub triples: Vec<Triple>,
}

impl Options {
    /// The run for `host`, serving `requested`.
    ///
    /// # Errors
    /// Refused on non-Linux hosts; usage error for non-Linux targets.
    pub fn new(_ctx: &Ctx, host: Triple, dry_run: bool, requested: &[Triple]) -> Result<Self> {
        if host.os() != Os::Linux {
            return Err(Error::refused(format!(
                "this lane packs Linux rows and this box is `{host}` — run it on Linux"
            )));
        }
        if let Some(t) = requested.iter().find(|t| t.os() != Os::Linux) {
            return Err(Error::usage(format!(
                "`{t}` is not a Linux target — this lane stages the Linux rows only"
            )));
        }
        let triples = if requested.is_empty() {
            Triple::all().filter(|t| t.os() == Os::Linux).collect()
        } else {
            requested.to_vec()
        };
        Ok(Self {
            dry_run,
            host,
            triples,
        })
    }
}

/// What this builder can produce.
pub trait Builder {
    /// Whether it can build `t` (not its host), else why not.
    ///
    /// # Errors
    /// The reason, for the table and `READY.md`.
    fn can_build(&self, t: Triple) -> std::result::Result<(), String>;

    /// The build environment a cross target needs (cargo's own linker variable).
    ///
    /// # Errors
    /// The reason it vanished since [`Builder::can_build`].
    fn env_for(&self, t: Triple) -> std::result::Result<Vec<(String, String)>, String>;
}

/// What the lane reaches beyond the context.
pub struct Deps<'a> {
    /// Packs plain programs.
    pub packer: &'a dyn Packer,
    /// Publishes the follow-up index.
    pub indexer: &'a dyn Indexer,
    /// What this builder can produce.
    pub builder: &'a dyn Builder,
    /// The lineage receipt switch, read live ([`live_switch`]).
    pub switch: &'a dyn Fn(&Ctx) -> Result<Switch>,
}

/// The switch from the trust checkout (`$HOME/trust`) — what the packs read.
///
/// # Errors
/// What the switch answers.
pub fn live_switch(ctx: &Ctx) -> Result<Switch> {
    lineage_switch::read(ctx, &lineage_switch::default_trust_repo(ctx))
}

/// Production Linux builder ([`tarwrite::COMPRESSOR`]).
#[derive(Debug)]
pub struct TrustBuilder {
    trustc: PathBuf,
}

impl TrustBuilder {
    /// The builder over the atpkg store's Trust seal.
    #[must_use]
    pub fn new(ctx: &Ctx) -> Self {
        Self {
            trustc: atpkg::toolchain::store_stage2_bin(&ctx.pkg_prefix).join("trustc"),
        }
    }

    fn linker(t: Triple) -> Option<PathBuf> {
        let arch = t.as_str().split('-').next().unwrap_or("");
        let path = std::env::var_os("PATH")?;
        [
            format!("{arch}-linux-gnu-gcc"),
            format!("{arch}-linux-gnu-cc"),
        ]
        .iter()
        .find_map(|name| {
            std::env::split_paths(&path)
                .map(|d| d.join(name))
                .find(|p| atpkg::toolchain::is_executable_file(p))
        })
    }
}

impl Builder for TrustBuilder {
    fn can_build(&self, t: Triple) -> std::result::Result<(), String> {
        let out = aterm_uds::spawnfd::command(&self.trustc)
            .args(["--print", "target-libdir", "--target", t.as_str()])
            .output()
            .map_err(|e| {
                format!(
                    "the Trust seal's trustc ({}) did not run: {e}",
                    self.trustc.display()
                )
            })?;
        let dir = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
        let has_core = out.status.success()
            && std::fs::read_dir(&dir).is_ok_and(|entries| {
                entries.flatten().any(|e| {
                    let n = e.file_name();
                    let n = n.to_string_lossy();
                    n.starts_with("libcore-") && n.ends_with(".rlib")
                })
            });
        if !has_core {
            return Err(format!(
                "the Trust seal on this builder has no std for {t} (a capability gap in the seal: \
                 {})",
                self.trustc.display()
            ));
        }
        Self::linker(t).ok_or_else(|| {
            let arch = t.as_str().split('-').next().unwrap_or("");
            format!("no cross linker for {t} on this builder ({arch}-linux-gnu-gcc on PATH)")
        })?;
        tarwrite::compressor_on_path().map(|_| ())
    }

    fn env_for(&self, t: Triple) -> std::result::Result<Vec<(String, String)>, String> {
        let linker = Self::linker(t).ok_or_else(|| {
            format!("the {t} cross linker vanished between the probe and the pack")
        })?;
        Ok(vec![(
            format!(
                "CARGO_TARGET_{}_LINKER",
                t.as_str().replace('-', "_").to_ascii_uppercase()
            ),
            linker.display().to_string(),
        )])
    }
}

/// One (program, target) row's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowState {
    /// The live manifest carries the row: nothing to do.
    Published,
    /// An earlier run built it; it waits for the ceremony.
    Staged,
    /// A latched failure for that target (its siblings keep going).
    Blocked,
    /// Missing, and this builder can produce it.
    Absent,
    /// Missing, and this builder cannot produce it (a builder limit, not a fault).
    Skipped,
}

impl RowState {
    /// Its word in the table.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Published => "published",
            Self::Staged => "staged",
            Self::Blocked => "blocked",
            Self::Absent => "absent",
            Self::Skipped => "skipped",
        }
    }
}

/// A row's state — pure.
#[must_use]
pub const fn row_state(published: bool, staged: bool, latched: bool, buildable: bool) -> RowState {
    if published {
        RowState::Published
    } else if staged {
        RowState::Staged
    } else if latched {
        RowState::Blocked
    } else if buildable {
        RowState::Absent
    } else {
        RowState::Skipped
    }
}

/// What a pinned program is to this lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// Built here.
    Plain,
    /// A sysroot bundle.
    Bundle,
    /// Vendor-direct (`atpkg::vendor_direct::is_vendor`).
    Vendor,
    /// Not a [`crate::programs::PROGRAMS`] row.
    Unknown,
}

impl Class {
    fn of(name: &str, def: Option<&ProgramDef>) -> Self {
        if atpkg::vendor_direct::is_vendor(name) {
            return Self::Vendor;
        }
        match def.map(|d| d.shape) {
            Some(Shape::Plain { .. }) => Self::Plain,
            Some(Shape::Bundle(_)) => Self::Bundle,
            Some(Shape::Package(_)) => Self::Vendor,
            None => Self::Unknown,
        }
    }

    const fn word(self) -> &'static str {
        match self {
            Self::Plain => "plain",
            Self::Bundle => "bundle",
            Self::Vendor => "vendor",
            Self::Unknown => "unknown",
        }
    }
}

/// One pinned program, as this run sees it.
#[derive(Clone, Debug)]
pub struct Program {
    /// Its name.
    pub name: String,
    /// Its repo (the index's).
    pub repo: String,
    /// Its coherence group.
    pub group: Option<String>,
    /// What it is to this lane.
    pub class: Class,
    /// Its pinned build.
    pub build: Build,
    /// Why its published manifest could not be read, if it could not.
    pub unfetchable: Option<String>,
    /// Each served target's state.
    pub rows: BTreeMap<Triple, RowState>,
}

impl Program {
    fn in_state(&self, states: &[RowState]) -> Vec<Triple> {
        self.rows
            .iter()
            .filter(|(_, s)| states.contains(s))
            .map(|(t, _)| *t)
            .collect()
    }

    /// The targets to build this run.
    #[must_use]
    pub fn need(&self) -> Vec<Triple> {
        self.in_state(&[RowState::Absent])
    }
}

/// One program's decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Its published manifest could not be read.
    Error(String),
    /// Every served row is published.
    Ok,
    /// A sysroot bundle.
    ManualBundle,
    /// Vendor-direct: nothing to do.
    Vendor,
    /// Not a PROGRAMS row: a human decision.
    Unknown,
    /// Build these targets (and which are held by the group).
    Build {
        /// The held targets.
        held: Vec<Triple>,
    },
    /// Latched for some targets, nothing to build.
    Blocked,
    /// Staged, awaiting signing.
    Staged {
        /// The held targets.
        held: Vec<Triple>,
    },
    /// Published wherever this builder can serve it.
    Partial,
}

impl Decision {
    /// The table's first word.
    #[must_use]
    pub const fn verdict(&self) -> &'static str {
        match self {
            Self::Error(_) => "ERROR",
            Self::Ok => "OK",
            Self::ManualBundle => "MANUAL-BUNDLE",
            Self::Vendor => "VENDOR",
            Self::Unknown => "UNKNOWN",
            Self::Build { .. } => "BUILD",
            Self::Blocked => "BLOCKED",
            Self::Staged { .. } => "STAGED",
            Self::Partial => "PARTIAL",
        }
    }
}

/// The decision for one program, pure over its rows and the group holds.
#[must_use]
pub fn decide(p: &Program, holds: &Holds) -> Decision {
    if let Some(why) = &p.unfetchable {
        return Decision::Error(why.clone());
    }
    if p.class == Class::Vendor {
        return Decision::Vendor;
    }
    let held = |states: &[RowState]| -> Vec<Triple> {
        p.group.as_deref().map_or_else(Vec::new, |g| {
            p.in_state(states)
                .into_iter()
                .filter(|t| holds.is_held(g, Some(*t)))
                .collect()
        })
    };
    if p.rows.values().all(|s| *s == RowState::Published) {
        return Decision::Ok;
    }
    match p.class {
        Class::Bundle => return Decision::ManualBundle,
        Class::Unknown => return Decision::Unknown,
        _ => {}
    }
    if !p.need().is_empty() {
        return Decision::Build {
            held: held(&[RowState::Absent, RowState::Staged]),
        };
    }
    if !p.in_state(&[RowState::Blocked]).is_empty() {
        return Decision::Blocked;
    }
    if !p.in_state(&[RowState::Staged]).is_empty() {
        return Decision::Staged {
            held: held(&[RowState::Absent, RowState::Staged]),
        };
    }
    Decision::Partial
}

/// Narrow `can` to the targets that resolve every program to the same build as `reference`.
#[must_use]
pub fn narrow(
    channel: &atpkg::Channel,
    can: &[Triple],
    reference: Triple,
    head: u64,
) -> (Vec<Triple>, Vec<(Triple, String)>) {
    let pins_of = |t: Triple| channel.for_target(t.as_str()).pin;
    if crate::pins::for_targets(channel, can).is_ok() {
        return (can.to_vec(), Vec::new());
    }
    let reference_pins = pins_of(reference);
    let mut kept = vec![reference];
    let mut dropped = Vec::new();
    for t in can.iter().copied().filter(|t| *t != reference) {
        if pins_of(t) == reference_pins {
            kept.push(t);
        } else {
            dropped.push((
                t,
                format!(
                    "index-{head} resolves {t} to a different build than {reference} — one \
                     manifest cannot carry both; that target needs its own run (--triple {t})"
                ),
            ));
        }
    }
    (kept, dropped)
}

/// What one run did.
#[derive(Clone, Debug, Default)]
pub struct Outcome {
    /// Another run held the lock.
    pub covered: bool,
    /// The verified head.
    pub head: Option<u64>,
    /// The targets this run could pack.
    pub can: Vec<Triple>,
    /// The targets it could not, and why.
    pub cannot: Vec<(Triple, String)>,
    /// The programs and their decisions.
    pub table: Vec<(Program, Decision)>,
    /// Programs that staged a row this run.
    pub staged: Vec<String>,
    /// Programs READY to sign.
    pub ready: Vec<String>,
    /// Programs held by their group.
    pub held: Vec<String>,
    /// Programs with staged rows that cannot be signed this run.
    pub unsignable: Vec<String>,
    /// Programs signed and uploaded this run.
    pub signed: Vec<String>,
    /// The follow-up index this run published.
    pub published: Option<u64>,
}

/// One run of the Linux lane.
///
/// # Errors
/// A refusal for a downgrade, a refused signing step, or a run that finished with
/// BLOCKED/ERROR rows; a could-not-run when the channel head cannot be read.
pub fn run(ctx: &Ctx, opts: &Options, deps: &Deps<'_>, tx: &Transcript) -> Result<Outcome> {
    let state = LaneState::of(LaneKind::Linux, &ctx.state_dir);
    state.ensure()?;
    let _guard = match state.try_lock()? {
        Lock::Held(g) => g,
        Lock::Busy => {
            if let Some(started) = state.stalled_since(ctx.now_unix) {
                return Err(Error::refused(format!(
                    "the run holding {} started {} — a stalled run; find it with `pgrep -fl \
                     'atpkg-publish lane run linux'` and kill it",
                    state.lock.display(),
                    aterm_types::rfc3339::format_rfc3339(u64::try_from(started).unwrap_or(0))
                )));
            }
            tx.say(format!(
                "another instance holds {} — its run covers this one; exiting 0",
                state.lock.display()
            ));
            return Ok(Outcome {
                covered: true,
                ..Outcome::default()
            });
        }
    };
    if !opts.dry_run {
        state.mark_started(ctx.now_unix);
    }
    let result = Run {
        ctx,
        opts,
        deps,
        state: &state,
        tx,
        failures: false,
    }
    .go();
    if !opts.dry_run {
        state.clear_started();
    }
    result
}

struct Run<'a> {
    ctx: &'a Ctx,
    opts: &'a Options,
    deps: &'a Deps<'a>,
    state: &'a LaneState,
    tx: &'a Transcript,
    failures: bool,
}

/// What a run knows about one program beyond its [`Program`] row.
struct Detail {
    /// The published manifest, downloaded.
    published: Option<PathBuf>,
    /// The published manifest's targets.
    published_rows: BTreeSet<Triple>,
}

impl Run<'_> {
    #[allow(clippy::too_many_lines)]
    fn go(&mut self) -> Result<Outcome> {
        let (ctx, opts) = (self.ctx, self.opts);
        self.tx.raw(format!(
            "=== {ME} {} pid {}{} ===",
            aterm_types::rfc3339::format_rfc3339(u64::try_from(ctx.now_unix).unwrap_or(0)),
            std::process::id(),
            if opts.dry_run { " (DRY_RUN)" } else { "" }
        ));
        if !opts.dry_run && !std::fs::metadata(&ctx.tokens.staging).is_ok_and(|m| m.len() > 0) {
            return Err(Error::cannot_run(format!(
                "staging token file missing/empty: {} — the lane reads the private staging \
                 repos as that identity, never anonymously",
                ctx.shown(&ctx.tokens.staging)
            )));
        }
        let mut can = Vec::new();
        let mut cannot: Vec<(Triple, String)> = Vec::new();
        for t in &opts.triples {
            if *t == opts.host {
                can.push(*t);
            } else {
                match self.deps.builder.can_build(*t) {
                    Ok(()) => can.push(*t),
                    Err(why) => cannot.push((*t, why)),
                }
            }
        }
        self.tx.say(format!(
            "builder is {}; atpkg serves {}; this box can pack {}",
            opts.host,
            names(&opts.triples),
            if can.is_empty() {
                "<none>".into()
            } else {
                names(&can)
            }
        ));
        for (t, why) in &cannot {
            self.tx.say(format!(
                "skipped target {t} — {why}; its rows must come from a builder that has it"
            ));
        }
        if can.is_empty() {
            return Err(Error::refused(format!(
                "this builder can produce NONE of {} — nothing to stage",
                names(&opts.triples)
            )));
        }

        let work = self.state.root.join("work-atpkg");
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work)
            .map_err(|e| Error::io(format!("mkdir {}", work.display()), &e))?;
        let slug = ctx.index_slug();
        let (n, head) = quad::fetch_head(ctx, &slug, &work.join("head"))?;
        crate::artifact::sign::elect_from_head(ctx, &head);
        self.state.check_index(n)?;
        self.tx.say(format!(
            "channel index head: atpkg-index-{n} (last processed: {})",
            self.state.index_seen()?
        ));
        let channel = head.index.channels.first().cloned().ok_or_else(|| {
            Error::refused(format!(
                "index-{n} carries no [[channels]] block — no pins to serve"
            ))
        })?;
        let reference = if can.contains(&opts.host) {
            opts.host
        } else {
            can[0]
        };
        let (kept, dropped) = narrow(&channel, &can, reference, n);
        for (t, why) in dropped {
            self.tx
                .warn(format!("narrowing this run to {reference} — {why}"));
            cannot.push((t, why));
        }
        let can = kept;
        let pins = crate::pins::for_targets(&channel, &can)?;
        self.state.check_pins(&can, &pins, n, self.tx)?;
        let groups = super::hold::groups_of(&head.index, pins.keys().map(String::as_str))?;

        let staged_dir = self.state.root.join(format!("staged-index-{n}"));
        let mut programs: Vec<Program> = Vec::new();
        let mut detail: BTreeMap<String, Detail> = BTreeMap::new();
        for (name, build) in ordered(&pins) {
            let def = programs::get(&name);
            let class = Class::of(&name, def);
            let index_row = head.index.programs.get(&name);
            let repo = index_row
                .map(|p| p.repo.clone())
                .or_else(|| def.map(|d| d.repo.to_string()))
                .unwrap_or_else(|| name.clone());
            let group = groups.get(&name).cloned().flatten();
            let mut p = Program {
                name: name.clone(),
                repo: repo.clone(),
                group,
                class,
                build,
                unfetchable: None,
                rows: BTreeMap::new(),
            };
            let mut d = Detail {
                published: None,
                published_rows: BTreeSet::new(),
            };
            if class != Class::Vendor {
                let sl = ctx.staging_slug(&repo);
                match rows::fetch(
                    &ctx.hosts.staging,
                    &sl,
                    &name,
                    build,
                    &work.join("pkg"),
                    &head,
                ) {
                    Fetched::Present { manifest, path } => {
                        d.published_rows = rows::targets(&manifest);
                        d.published = Some(path);
                    }
                    Fetched::Absent(why) | Fetched::Unreachable(why) | Fetched::Invalid(why) => {
                        p.unfetchable = Some(format!(
                            "cannot fetch {} from release {} on {sl}: {why}",
                            pkgdoc::manifest_name(&name, build),
                            upload::tag(&name, build)
                        ));
                    }
                }
                let staged_rows = self.staged_rows(&staged_dir, &name, build);
                for t in &opts.triples {
                    let latched = staged_dir
                        .join(format!("BLOCKED-{name}-{build}-{t}.md"))
                        .exists()
                        || staged_dir
                            .join(format!("BLOCKED-{name}-{build}.md"))
                            .exists();
                    p.rows.insert(
                        *t,
                        row_state(
                            d.published_rows.contains(t),
                            staged_rows.contains(t),
                            latched,
                            can.contains(t),
                        ),
                    );
                }
            }
            programs.push(p);
            detail.insert(name, d);
        }
        let mut holds = Holds::new();
        for p in &programs {
            if let (Some(g), true) = (&p.group, p.class != Class::Plain) {
                for t in &opts.triples {
                    if p.rows.get(t) != Some(&RowState::Published) {
                        holds.not_ready(g, Some(*t), &p.name);
                    }
                }
            }
        }
        let decisions: Vec<Decision> = programs.iter().map(|p| decide(p, &holds)).collect();
        if decisions.iter().any(|d| matches!(d, Decision::Error(_))) {
            self.failures = true;
        }
        self.say_table(n, &channel.name, &head, &programs, &decisions, &staged_dir);
        let build_list: Vec<usize> = decisions
            .iter()
            .enumerate()
            .filter(|(_, d)| matches!(d, Decision::Build { .. }))
            .map(|(i, _)| i)
            .collect();
        let mut outcome = Outcome {
            head: Some(n),
            can: can.clone(),
            cannot: cannot.clone(),
            ..Outcome::default()
        };
        if opts.dry_run {
            self.tx.say(format!(
                "DRY_RUN — would build: {}; no clone/build/upload/state change",
                list(build_list.iter().map(|i| programs[*i].name.as_str()))
            ));
            outcome.table = programs.into_iter().zip(decisions).collect();
            return Ok(outcome);
        }

        for &i in &build_list {
            let p = &mut programs[i];
            let published = detail.get(&p.name).and_then(|d| d.published.clone());
            if self.stage(p, published.as_deref(), &staged_dir, &work, n)? {
                outcome.staged.push(p.name.clone());
            }
        }

        for p in &programs {
            let Some(g) = &p.group else { continue };
            let staged_rows = self.staged_rows(&staged_dir, &p.name, p.build);
            for t in &opts.triples {
                if p.rows.get(t) != Some(&RowState::Published) && !staged_rows.contains(t) {
                    holds.not_ready(g, Some(*t), &p.name);
                }
            }
        }
        let mut ready: Vec<(&Program, Vec<Triple>)> = Vec::new();
        let mut held: Vec<(&Program, Vec<Triple>)> = Vec::new();
        let mut signed_earlier: Vec<&Program> = Vec::new();
        let mut unsignable: Vec<(&Program, String)> = Vec::new();
        for p in &programs {
            if p.class != Class::Plain {
                continue;
            }
            let staged = staged_manifest(&staged_dir, &p.name, p.build);
            if let Some(why) = &p.unfetchable {
                if staged.exists() {
                    self.tx.warn(format!(
                        "{}@{}: its staged rows are NOT signable this run — {why}",
                        p.name, p.build
                    ));
                    unsignable.push((p, why.clone()));
                }
                continue;
            }
            let d = &detail[&p.name];
            let new: Vec<Triple> = self
                .staged_rows(&staged_dir, &p.name, p.build)
                .into_iter()
                .filter(|t| opts.triples.contains(t) && !d.published_rows.contains(t))
                .collect();
            if new.is_empty() {
                // Signed earlier only when the staging owner serves exactly what this lane
                // signed: a manifest another machine extended since is not this lane's.
                if staged.with_extension("toml.sig").exists()
                    && same_bytes(&staged, d.published.as_deref())
                {
                    signed_earlier.push(p);
                }
                continue;
            }
            let is_held = p
                .group
                .as_deref()
                .is_some_and(|g| new.iter().any(|t| holds.is_held(g, Some(*t))));
            if is_held {
                held.push((p, new));
            } else {
                ready.push((p, new));
            }
        }
        outcome.ready = ready.iter().map(|(p, _)| p.name.clone()).collect();
        outcome.held = held.iter().map(|(p, _)| p.name.clone()).collect();
        outcome.unsignable = unsignable.iter().map(|(p, _)| p.name.clone()).collect();

        let have_key = ctx.identity.key.is_file();
        let ceremony = ctx.shown(&staged_dir.join("READY.md"));
        let mut ready_md = Ready {
            ctx,
            staged_dir: &staged_dir,
            n,
            can: &can,
            cannot: &cannot,
            ready: &ready,
            held: &held,
            signed_earlier: &signed_earlier,
            unsignable: &unsignable,
            have_key,
            refusal: None,
        };
        let publish = have_key && (!ready.is_empty() || !signed_earlier.is_empty());
        if publish {
            let token = std::fs::metadata(&ctx.tokens.channel).is_ok_and(|m| m.len() > 0);
            if !token {
                ready_md.refusal = Some(format!(
                    "the channel token at {} is missing or empty, and the public uploads need \
                     it.\nPut it there, then run the ceremony below.",
                    ctx.shown(&ctx.tokens.channel)
                ));
                ready_md.write(Phase::Staged)?;
                return Err(Error::refused(format!(
                    "channel token file missing/empty: {} (the public uploads need it) — nothing \
                     signed; ceremony: {ceremony}",
                    ctx.shown(&ctx.tokens.channel),
                )));
            }
            let key = match crate::identity::signer_for(ctx, &head.roster) {
                Ok(signer) => signer.key,
                Err(e) => {
                    ready_md.refusal = Some(format!(
                        "{e}.\nA live machine of the roster finishes it with the ceremony below."
                    ));
                    ready_md.write(Phase::Staged)?;
                    return Err(Error::refused(format!(
                        "{e} — nothing signed; ceremony: {ceremony}"
                    )));
                }
            };
            let recheck = work.join("recheck");
            let mut switch: Option<std::result::Result<Switch, String>> = None;
            for (p, new) in &ready {
                if let Err(e) = self.signable(p, new, &head, &recheck, &staged_dir, &mut switch) {
                    let _ = std::fs::remove_file(staged_dir.join("READY.md"));
                    return Err(e);
                }
            }
            for (p, _) in &ready {
                if let Err(e) = self.sign_verified(&key, p, &head, &staged_dir) {
                    let _ = std::fs::remove_file(staged_dir.join("READY.md"));
                    return Err(e);
                }
            }
            ready_md.write(Phase::Signing)?;
            if !ready.is_empty() {
                self.tx.say(format!(
                    "machine key present — signing and publishing {} program(s)",
                    ready.len()
                ));
            }
            for (p, new) in &ready {
                self.upload(p, new, &staged_dir)?;
                outcome.signed.push(p.name.clone());
            }
            for p in &signed_earlier {
                self.converge_public(p, &head, &recheck, &staged_dir)?;
            }
            let published = self
                .deps
                .indexer
                .publish(
                    ctx,
                    &IndexRequest {
                        baseline: Baseline::Dir(head.dir.clone()),
                        delta: Delta::default(),
                        out: staged_dir.join("index-out"),
                        upload: true,
                        pack_dir: None,
                    },
                )
                .map_err(|e| {
                    Error::refused(format!(
                        "the follow-up index {} did not publish ({e}) — {ceremony} carries the \
                         ceremony",
                        n + 1,
                    ))
                })?;
            self.tx.say(format!(
                "published atpkg-index-{} to {slug}",
                published.index_build
            ));
            outcome.published = Some(published.index_build);
            ready_md.write(Phase::Published)?;
        } else {
            ready_md.write(Phase::Staged)?;
            if !ready.is_empty() {
                self.tx.say(format!(
                    "no machine key at {} — staged UNSIGNED under {} (ceremony: {ceremony})",
                    ctx.shown(&ctx.identity.key),
                    ctx.shown(&staged_dir),
                ));
            } else if !held.is_empty() {
                let holds = &holds;
                let waits: BTreeSet<String> = held
                    .iter()
                    .filter_map(|(p, new)| {
                        let g = p.group.as_deref()?;
                        Some(new.iter().filter_map(move |t| holds.holding(g, Some(*t))))
                    })
                    .flatten()
                    .flatten()
                    .collect();
                self.tx.say(format!(
                    "held unsigned: {} — waiting on a Linux row for {} (a coherence group \
                     publishes whole)",
                    list(held.iter().map(|(p, _)| p.name.as_str())),
                    waits.into_iter().collect::<Vec<_>>().join(", ")
                ));
            }
            if !signed_earlier.is_empty() {
                self.tx.say(format!(
                    "atpkg-index-{} unpublished: an earlier run signed and uploaded {} — its step \
                     is in {ceremony}",
                    n + 1,
                    signed_earlier
                        .iter()
                        .map(|p| p.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                ));
            }
        }

        let index_floor = n;
        self.state.record(index_floor, &can, &pins)?;
        let staged_names = outcome.staged.clone();
        outcome.table = programs.clone().into_iter().zip(decisions).collect();
        if self.failures {
            return Err(Error::refused(format!(
                "run finished with BLOCKED/ERROR rows — see the table above and the markers under \
                 {}",
                ctx.shown(&staged_dir)
            )));
        }
        self.tx.say(format!(
            "done — staged this run: {}; ready: {}; hold: {}",
            list(staged_names.iter().map(String::as_str)),
            list(outcome.ready.iter().map(String::as_str)),
            list(outcome.held.iter().map(String::as_str)),
        ));
        Ok(outcome)
    }

    /// The targets this lane staged for `name` `build`; none when there is no staged manifest.
    fn staged_rows(&self, staged_dir: &Path, name: &str, build: Build) -> BTreeSet<Triple> {
        let path = staged_manifest(staged_dir, name, build);
        if !path.exists() {
            return BTreeSet::new();
        }
        match rows::local(&path) {
            Ok(m) => rows::targets(&m),
            Err(e) => {
                self.tx.warn(format!("{e} — read as staging nothing"));
                BTreeSet::new()
            }
        }
    }

    fn say_table(
        &self,
        n: u64,
        channel: &str,
        head: &VerifiedQuad,
        programs: &[Program],
        decisions: &[Decision],
        staged_dir: &Path,
    ) {
        self.tx.say(format!(
            "decision table for atpkg-index-{n} (channel '{channel}', valid_until {}):",
            head.index.valid_until
        ));
        let staged_name = staged_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut table = vec![vec![
            "PROGRAM".to_string(),
            "CLASS".into(),
            "PIN".into(),
            "LINUX-ROWS".into(),
            "DECISION".into(),
        ]];
        for (p, d) in programs.iter().zip(decisions) {
            let cell = if p.unfetchable.is_some() {
                "unfetchable".to_string()
            } else if p.rows.is_empty() {
                "-".to_string()
            } else {
                p.rows
                    .iter()
                    .map(|(t, s)| {
                        format!(
                            "{}={}",
                            t.as_str().split('-').next().unwrap_or(""),
                            s.word()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            table.push(vec![
                p.name.clone(),
                p.class.word().into(),
                p.build.to_string(),
                cell,
                render(p, d, &staged_name),
            ]);
        }
        for line in report::columns(&table) {
            self.tx.raw(line);
        }
    }

    fn block(
        &mut self,
        staged_dir: &Path,
        p: &Program,
        triple: Option<Triple>,
        headline: &str,
        n: u64,
    ) {
        let (name, build) = (&p.name, p.build);
        let marker = staged_dir.join(match triple {
            Some(t) => format!("BLOCKED-{name}-{build}-{t}.md"),
            None => format!("BLOCKED-{name}-{build}.md"),
        });
        let which = triple.map(|t| format!(" ({t})")).unwrap_or_default();
        let text = format!(
            "# linux-auto-atpkg: {name}@{build}{which} BLOCKED\n\n{headline}\n\n- pinned by \
             atpkg-index-{n}\n- this file is the latch: the lane skips {name}@{build}{} without \
             rebuilding while it exists. Delete it to retry.\n",
            triple.map(|t| format!(" for {t}")).unwrap_or_default()
        );
        let _ = report::write_atomic(&marker, &text);
        self.tx.warn(format!(
            "{name}@{build}{which} BLOCKED — {headline} (marker: {})",
            self.ctx.shown(&marker)
        ));
        self.failures = true;
    }

    /// Build and pack every absent target of `p`, chaining each merge onto the last.
    fn stage(
        &mut self,
        p: &mut Program,
        published: Option<&Path>,
        staged_dir: &Path,
        work: &Path,
        n: u64,
    ) -> Result<bool> {
        let ctx = self.ctx;
        let (name, build) = (p.name.clone(), p.build);
        let slug = ctx.staging_slug(&p.repo);
        let clone = self.state.root.join("progs").join(&p.repo);
        self.tx
            .say(format!("[{name}@{build}] preparing source clone of {slug}"));
        let prepared = if clone.join(".git").exists() {
            ctx.hosts
                .staging
                .fetch_repo(&slug, &clone)
                .map_err(|e| format!("git fetch failed in the {} clone: {e}", p.repo))
        } else {
            std::fs::create_dir_all(self.state.root.join("progs"))
                .map_err(|e| e.to_string())
                .and_then(|()| {
                    ctx.hosts
                        .staging
                        .clone_repo(&slug, &clone)
                        .map_err(|e| format!("clone of {slug} failed: {e}"))
                })
        };
        if let Err(why) = prepared {
            self.block(staged_dir, p, None, &why, n);
            return Ok(false);
        }
        let sha = match super::resolve::resolve_commit(&clone, build) {
            Ok(sha) => sha,
            Err(e) => {
                self.block(
                    staged_dir,
                    p,
                    None,
                    &format!("could not resolve pin {build} to a commit — {e}"),
                    n,
                );
                return Ok(false);
            }
        };
        self.tx
            .say(format!("[{name}@{build}] pinned commit is {sha}"));
        if let Err(e) = crate::git::run(&clone, &["checkout", "--force", "--detach", &sha]) {
            self.block(
                staged_dir,
                p,
                None,
                &format!("checkout of {sha} failed: {e}"),
                n,
            );
            return Ok(false);
        }
        let dir = staged_dir.join(&name);
        let staged = staged_manifest(staged_dir, &name, build);
        let base = work.join(format!("base-{name}.toml"));
        let from = if staged.exists() {
            Some(staged.clone())
        } else {
            published.map(Path::to_path_buf)
        };
        let Some(from) = from else {
            return Ok(false);
        };
        std::fs::copy(&from, &base)
            .map_err(|e| Error::io(format!("copy {}", from.display()), &e))?;
        let mut any = false;
        for t in p.need() {
            let env = if t == self.opts.host {
                Vec::new()
            } else {
                match self.deps.builder.env_for(t) {
                    Ok(env) => env,
                    Err(why) => {
                        self.block(staged_dir, p, Some(t), &why, n);
                        p.rows.insert(t, RowState::Blocked);
                        continue;
                    }
                }
            };
            let out = staged_dir.join(".work").join(&name).join(t.as_str());
            let packed = self.deps.packer.pack(
                ctx,
                &PackRequest {
                    program: name.clone(),
                    checkout: clone.clone(),
                    triple: t,
                    build: Some(build),
                    merge_from: Some(base.clone()),
                    toolchain: None,
                    out: out.clone(),
                    signing: Signing::Unsigned,
                    upload: false,
                    env,
                },
            );
            let outcome = match packed {
                Ok(o) => o,
                Err(e) => {
                    self.block(
                        staged_dir,
                        p,
                        Some(t),
                        &format!("the {t} pack failed: {e}"),
                        n,
                    );
                    p.rows.insert(t, RowState::Blocked);
                    continue;
                }
            };
            let carries = rows::local(&outcome.manifest).map(|m| rows::targets(&m).contains(&t));
            if outcome.build != build || !matches!(carries, Ok(true)) {
                self.block(
                    staged_dir,
                    p,
                    Some(t),
                    &format!("the merged manifest has no {t} row for build {build} — pack drift"),
                    n,
                );
                p.rows.insert(t, RowState::Blocked);
                continue;
            }
            std::fs::create_dir_all(&dir)
                .map_err(|e| Error::io(format!("mkdir {}", dir.display()), &e))?;
            let asset_name = outcome
                .asset
                .file_name()
                .map(|n| n.to_os_string())
                .ok_or_else(|| Error::refused("the pack's asset names no file"))?;
            move_file(&outcome.asset, &dir.join(asset_name))?;
            std::fs::copy(&outcome.manifest, &staged)
                .map_err(|e| Error::io(format!("stage {}", staged.display()), &e))?;
            std::fs::copy(&outcome.manifest, &base)
                .map_err(|e| Error::io(format!("copy {}", base.display()), &e))?;
            let _ = std::fs::remove_file(staged.with_extension("toml.sig"));
            let _ = std::fs::remove_dir_all(&out);
            p.rows.insert(t, RowState::Staged);
            any = true;
            self.tx.say(format!(
                "[{name}@{build}] staged {t}: {}/{name}/ (unsigned)",
                staged_dir
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            ));
        }
        Ok(any)
    }

    /// The gates a ready manifest passes before the key touches it.
    fn signable(
        &self,
        p: &Program,
        new: &[Triple],
        head: &VerifiedQuad,
        recheck: &Path,
        staged_dir: &Path,
        switch: &mut Option<std::result::Result<Switch, String>>,
    ) -> Result<()> {
        let ctx = self.ctx;
        let (name, build) = (&p.name, p.build);
        let dir = staged_dir.join(name);
        let staged = staged_manifest(staged_dir, name, build);
        let staged_bytes = std::fs::read(&staged)
            .map_err(|e| Error::io(format!("read {}", staged.display()), &e))?;
        for (owner, slug) in self.owners(p) {
            let (published, _) = self.published_now(p, owner, &slug, head, recheck)?;
            if !staged_bytes.starts_with(&published) {
                return Err(Error::refused(format!(
                    "{name}@{build}: the published manifest on {slug} is no longer a byte-prefix \
                     of the staged merge — another machine extended it; delete {} and re-run to \
                     restage",
                    ctx.shown(&dir)
                )));
            }
        }
        let manifest: PkgManifest = rows::local(&staged)?;
        for t in new {
            let asset = manifest.artifact_for(t.as_str()).map(|a| a.asset.clone());
            if asset.as_ref().is_none_or(|a| !dir.join(a).is_file()) {
                return Err(Error::refused(format!(
                    "{name}@{build}: the staged manifest carries a {t} row but its tarball ({}) is \
                     missing — delete {} and re-run to restage",
                    asset.unwrap_or_default(),
                    ctx.shown(&dir)
                )));
            }
        }
        let unproven = rows::unproven(&manifest);
        if new.iter().any(|t| unproven.contains(t)) {
            let read =
                switch.get_or_insert_with(|| (self.deps.switch)(ctx).map_err(|e| e.to_string()));
            let what = format!(
                "{name}@{build}: the staged manifest's new rows were packed by a compiler with no \
                 lineage receipt (lineage = \"unproven\"), admitted while the receipt switch was \
                 dormant"
            );
            let trust = ctx.shown(&lineage_switch::default_trust_repo(ctx));
            match read {
                Ok(Switch::Dormant) => {}
                Ok(Switch::Live) => {
                    return Err(Error::refused(format!(
                        "{what}, and {trust}'s origin main now carries a minted re-pin — nothing \
                         signed; delete {} and re-run to restage with a receipt-carrying compiler",
                        ctx.shown(&dir)
                    )));
                }
                Ok(Switch::Unknown(why)) | Err(why) => {
                    return Err(Error::refused(format!(
                        "{what}, and nothing shows it still is ({why}) — nothing signed; delete \
                         {} and re-run to restage, or re-run once that is fixed to sign these \
                         rows as staged",
                        ctx.shown(&dir)
                    )));
                }
            }
        }
        Ok(())
    }

    /// The two owners a signed manifest is uploaded to, each with the host that speaks for it.
    fn owners(&self, p: &Program) -> [(Owner, Slug); 2] {
        [
            (Owner::Staging, self.ctx.staging_slug(&p.repo)),
            (Owner::Public, self.ctx.public_slug(&p.repo)),
        ]
    }

    /// `p`'s manifest as `slug` publishes it now, verified under the head's roster.
    fn published_now(
        &self,
        p: &Program,
        owner: Owner,
        slug: &Slug,
        head: &VerifiedQuad,
        recheck: &Path,
    ) -> Result<(Vec<u8>, PkgManifest)> {
        let (name, build) = (&p.name, p.build);
        let host = match owner {
            Owner::Staging => &self.ctx.hosts.staging,
            Owner::Public => &self.ctx.hosts.channel,
        };
        let dir = recheck.join(&slug.owner).join(name);
        let _ = std::fs::remove_dir_all(&dir);
        match rows::fetch(host, slug, name, build, &dir, head) {
            Fetched::Present { manifest, path } => {
                let bytes = std::fs::read(&path)
                    .map_err(|e| Error::io(format!("read {}", path.display()), &e))?;
                Ok((bytes, manifest))
            }
            Fetched::Absent(why) | Fetched::Unreachable(why) | Fetched::Invalid(why) => {
                Err(Error::refused(format!(
                    "{name}@{build}: no verified published manifest on {slug} to prove the \
                     byte-prefix extension against ({why}) — nothing signed"
                )))
            }
        }
    }

    /// Sign the staged manifest and verify the pair against the channel head's roster.
    fn sign_verified(
        &self,
        key: &crate::identity::MachineKey,
        p: &Program,
        head: &VerifiedQuad,
        staged_dir: &Path,
    ) -> Result<()> {
        let (name, build) = (&p.name, p.build);
        let staged = staged_manifest(staged_dir, name, build);
        let sig = key.sign_file(&staged)?;
        let verified = std::fs::read(&staged)
            .and_then(|bytes| Ok((bytes, std::fs::read(&sig)?)))
            .map_err(|e| Error::io(format!("read back {}", staged.display()), &e))
            .and_then(|(bytes, signature)| verify_as(head, &bytes, &signature, p));
        if let Err(e) = verified {
            let _ = std::fs::remove_file(&sig);
            return Err(Error::refused(format!(
                "{name}@{build}: the signed manifest does not verify through the client's chain \
                 ({e}) — nothing uploaded"
            )));
        }
        Ok(())
    }

    /// Upload a signed manifest, and the tarballs of its new rows, to both owners.
    fn upload(&self, p: &Program, new: &[Triple], staged_dir: &Path) -> Result<()> {
        let ctx = self.ctx;
        let (name, build) = (&p.name, p.build);
        let staged = staged_manifest(staged_dir, name, build);
        let manifest = rows::local(&staged)?;
        let mut files = tarballs(&manifest, new, &staged_dir.join(name));
        files.push(staged.clone());
        files.push(staged.with_extension("toml.sig"));
        let tag = upload::tag(name, build);
        for (owner, slug) in self.owners(p) {
            let host = match owner {
                Owner::Staging => &ctx.hosts.staging,
                Owner::Public => &ctx.hosts.channel,
            };
            host.upload(&slug, &tag, &files, Replace::Clobber)
                .map_err(|e| {
                    Error::refused(format!(
                        "{name}@{build}: the upload to {slug} {tag} failed ({e}) — {} carries the \
                         ceremony",
                        ctx.shown(&staged_dir.join("READY.md"))
                    ))
                })?;
        }
        self.tx.say(format!(
            "[{name}@{build}] signed + uploaded {} to both owners",
            new.iter()
                .map(|t| t.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
        Ok(())
    }

    /// An earlier run signed and uploaded `p` to staging.
    fn converge_public(
        &self,
        p: &Program,
        head: &VerifiedQuad,
        recheck: &Path,
        staged_dir: &Path,
    ) -> Result<()> {
        let ctx = self.ctx;
        let (name, build) = (&p.name, p.build);
        let staged = staged_manifest(staged_dir, name, build);
        let sig = staged.with_extension("toml.sig");
        let read = |path: &Path| {
            std::fs::read(path).map_err(|e| Error::io(format!("read {}", path.display()), &e))
        };
        let (staged_bytes, signature) = (read(&staged)?, read(&sig)?);
        let slug = ctx.public_slug(&p.repo);
        let (public, public_manifest) =
            self.published_now(p, Owner::Public, &slug, head, recheck)?;
        if public == staged_bytes {
            return Ok(());
        }
        if !staged_bytes.starts_with(&public) {
            return Err(Error::refused(format!(
                "{name}@{build}: {slug} serves a manifest that is neither the one an earlier run \
                 signed nor a byte-prefix of it — nothing indexed; delete {} and re-run to restage",
                ctx.shown(&staged_dir.join(name))
            )));
        }
        let signed = verify_as(head, &staged_bytes, &signature, p).map_err(|e| {
            Error::refused(format!(
                "{name}@{build}: the pair an earlier run signed no longer verifies ({e}) — nothing \
                 indexed"
            ))
        })?;
        let lacking: Vec<Triple> = rows::targets(&signed)
            .difference(&rows::targets(&public_manifest))
            .copied()
            .collect();
        let mut files = tarballs(&signed, &lacking, &staged_dir.join(name));
        files.push(staged.clone());
        files.push(sig);
        let tag = upload::tag(name, build);
        ctx.hosts
            .channel
            .upload(&slug, &tag, &files, Replace::Clobber)
            .map_err(|e| {
                Error::refused(format!(
                    "{name}@{build}: the public upload an earlier run owed {slug} {tag} failed \
                     ({e}) — nothing indexed"
                ))
            })?;
        self.tx.say(format!(
            "[{name}@{build}] {slug} lacked the manifest an earlier run signed — uploaded it there"
        ));
        Ok(())
    }
}

/// Verify `bytes` and `signature` through the client's chain.
fn verify_as(
    head: &VerifiedQuad,
    bytes: &[u8],
    signature: &[u8],
    p: &Program,
) -> Result<PkgManifest> {
    let m = head.verify_pkg(bytes, signature)?;
    if m.program == p.name && m.build_number == p.build {
        Ok(m)
    } else {
        Err(Error::refused(format!(
            "it names {} build {}",
            m.program, m.build_number
        )))
    }
}

/// Which owner a signed manifest goes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Owner {
    Staging,
    Public,
}

/// The staged tarballs of `manifest`'s rows for `targets`.
fn tarballs(manifest: &PkgManifest, targets: &[Triple], dir: &Path) -> Vec<PathBuf> {
    targets
        .iter()
        .filter_map(|t| manifest.artifact_for(t.as_str()))
        .map(|a| dir.join(&a.asset))
        .collect()
}

/// Whether `a` holds exactly the bytes of `b` (false when either cannot be read).
fn same_bytes(a: &Path, b: Option<&Path>) -> bool {
    match (std::fs::read(a), b.map(std::fs::read)) {
        (Ok(a), Some(Ok(b))) => a == b,
        _ => false,
    }
}

fn render(p: &Program, d: &Decision, staged_name: &str) -> String {
    let triples = |states: &[RowState]| -> String {
        p.in_state(states)
            .iter()
            .map(|t| t.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut note = String::new();
    let blocked = triples(&[RowState::Blocked]);
    let skipped = triples(&[RowState::Skipped]);
    if !blocked.is_empty() {
        note.push_str(&format!(
            "; latched BLOCKED for {blocked} (delete the marker under {staged_name}/ to retry)"
        ));
    }
    let skip_note = if skipped.is_empty() {
        String::new()
    } else {
        format!("; not served by this builder: {skipped}")
    };
    note.push_str(&skip_note);
    let hold = |held: &[Triple]| -> String {
        if held.is_empty() {
            String::new()
        } else {
            format!(
                "+HOLD(group {}: {})",
                p.group.as_deref().unwrap_or(""),
                held.iter()
                    .map(|t| t.as_str().split('-').next().unwrap_or(""))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
    };
    match d {
        Decision::Error(why) => format!("ERROR: {why}"),
        Decision::Ok => format!("OK: rows published for {}", triples(&[RowState::Published])),
        Decision::ManualBundle => format!(
            "MANUAL-BUNDLE: a sysroot bundle — `atpkg-publish publish-group` / `pack-bundle` on a \
             builder with its sysroot (docs/RELEASING.md){note}"
        ),
        Decision::Vendor => {
            "VENDOR: vendor-direct — the client follows the vendor; no lane packs it".into()
        }
        Decision::Unknown => format!("UNKNOWN: not a PROGRAMS row — human decision{note}"),
        Decision::Build { held } => format!(
            "BUILD{}: resolve pinned commit, pack {}, merge manifest, stage{note}",
            hold(held),
            triples(&[RowState::Absent])
        ),
        Decision::Blocked => {
            format!(
                "BLOCKED: latched for {blocked} under {staged_name}/ (delete the marker to retry){skip_note}"
            )
        }
        Decision::Staged { held } => format!(
            "STAGED{}: awaiting signing (see {staged_name}/READY.md){note}",
            hold(held)
        ),
        Decision::Partial => format!(
            "PARTIAL: published {}{note} (no row there until a builder that has it runs)",
            triples(&[RowState::Published])
        ),
    }
}

/// The pinned programs in table order: [`crate::programs::PROGRAMS`] first.
fn ordered(pins: &BTreeMap<String, Build>) -> Vec<(String, Build)> {
    let mut out: Vec<(String, Build)> = programs::PROGRAMS
        .iter()
        .filter_map(|d| pins.get(d.name).map(|b| (d.name.to_string(), *b)))
        .collect();
    out.extend(
        pins.iter()
            .filter(|(p, _)| programs::get(p).is_none())
            .map(|(p, b)| (p.clone(), *b)),
    );
    out
}

/// This lane's staged manifest for `name` `build`.
#[must_use]
pub fn staged_manifest(staged_dir: &Path, name: &str, build: Build) -> PathBuf {
    staged_dir
        .join(name)
        .join(pkgdoc::manifest_name(name, build))
}

fn move_file(from: &Path, to: &Path) -> Result<()> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    std::fs::copy(from, to).map_err(|e| Error::io(format!("copy {}", from.display()), &e))?;
    let _ = std::fs::remove_file(from);
    Ok(())
}

fn list<'a>(items: impl IntoIterator<Item = &'a str>) -> String {
    let v: Vec<&str> = items.into_iter().collect();
    if v.is_empty() {
        "<none>".into()
    } else {
        v.join(" ")
    }
}

/// Which `READY.md` a run leaves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Staged; nothing signed by this run.
    Staged,
    /// Written before the key is used, so a run that stops half way leaves the ceremony.
    Signing,
    /// Signed, uploaded and indexed by this run.
    Published,
}

struct Ready<'a> {
    ctx: &'a Ctx,
    staged_dir: &'a Path,
    n: u64,
    can: &'a [Triple],
    cannot: &'a [(Triple, String)],
    ready: &'a [(&'a Program, Vec<Triple>)],
    held: &'a [(&'a Program, Vec<Triple>)],
    signed_earlier: &'a [&'a Program],
    unsignable: &'a [(&'a Program, String)],
    have_key: bool,
    /// Why a box holding the key signed nothing (said in place of the ceremony's preamble).
    refusal: Option<String>,
}

impl Ready<'_> {
    fn write(&self, phase: Phase) -> Result<()> {
        report::write_atomic(&self.staged_dir.join("READY.md"), &self.render(phase))
    }

    /// `READY.md` says what is true when it is read.
    fn render(&self, phase: Phase) -> String {
        let ctx = self.ctx;
        let (n, next) = (self.n, self.n + 1);
        let key = ctx.shown(&ctx.identity.key);
        let token = ctx.shown(&ctx.tokens.channel);
        let ready = list(self.ready.iter().map(|(p, _)| p.name.as_str()));
        let mut s = format!(
            "# READY — staged Linux artifacts for atpkg-index-{n}\n\n\
             Everything below was built at the EXACT pinned commit and merged onto the\n\
             PUBLISHED manifest bytes (darwin rows untouched, one new row appended per Linux\n\
             target, each merge chaining onto the last so ONE manifest carries them all).\n\n"
        );
        match phase {
            Phase::Published if self.ready.is_empty() => s.push_str(&format!(
                "Published atpkg-index-{next} for the uploads an earlier run signed: {}.\n",
                list(self.signed_earlier.iter().map(|p| p.name.as_str()))
            )),
            Phase::Published => s.push_str(&format!(
                "Signed and published by this run with the machine key at {key}:\n{ready}, then \
                 atpkg-index-{next}.\n"
            )),
            Phase::Signing => s.push_str(&format!(
                "A run started signing and publishing the READY programs below with the machine \
                 key at {key}.\nIf its log has no \"published atpkg-index-{next}\", run the \
                 ceremony below.\n"
            )),
            Phase::Staged => {
                let mut none = "Nothing is signed";
                if !self.signed_earlier.is_empty() {
                    s.push_str(&format!(
                        "Signed and uploaded by an earlier run: {}.\natpkg-index-{next} is not \
                         published yet: run the follow-up index below.\n",
                        list(self.signed_earlier.iter().map(|p| p.name.as_str()))
                    ));
                    none = "Nothing else is signed";
                }
                s.push_str(&match (self.have_key, self.ready.is_empty()) {
                    (true, false) => format!(
                        "{none}: {}\n",
                        self.refusal
                            .as_deref()
                            .unwrap_or("this run stopped before the key")
                    ),
                    (true, true) => {
                        format!(
                            "{none}: nothing is READY. This box holds the machine key at {key}.\n"
                        )
                    }
                    (false, false) => format!(
                        "{none}: this box has no machine key at {key}.\nA rostered machine \
                         finishes it with the ceremony below.\n"
                    ),
                    (false, true) => format!(
                        "{none}: nothing is READY, and this box has no machine key at {key}.\n"
                    ),
                });
            }
        }
        s.push_str(&format!(
            "\nLinux targets this builder could pack: {}\n",
            if self.can.is_empty() {
                "<none>".into()
            } else {
                names(self.can)
            }
        ));
        if !self.cannot.is_empty() {
            for (t, why) in self.cannot {
                s.push_str(&format!("  SKIPPED {t} — {why}\n"));
            }
            s.push_str(
                "  Those architectures keep reading 'unavailable on <target>' on their own\n  \
                 clients until a builder that HAS them runs this lane. The rows below are\n  \
                 unaffected: a builder limit never holds back a row this box can produce.\n",
            );
        }
        s.push('\n');
        if phase == Phase::Published {
            s.push_str(&format!("READY (published by this run): {ready}\n"));
        } else {
            s.push_str(&format!("READY (sign + publish now): {ready}\n"));
        }
        s.push_str(&format!(
            "HOLD  (do NOT publish yet): {}\n",
            list(self.held.iter().map(|(p, _)| p.name.as_str()))
        ));
        for (p, why) in self.unsignable {
            s.push_str(&format!(
                "NOT SIGNABLE this run: {} — {why} (nothing proves the staged merge still \
                 extends what is published)\n",
                p.name
            ));
        }
        if !self.held.is_empty() {
            s.push_str(
                "\nHOLD means the program's coherence group still lacks a Linux row (the trust\n\
                 sysroot bundle). The group moves all-or-nothing (§7): partial Linux rows ABORT\n\
                 it on every Linux client, where no rows is a clean skip. Sign these only\n\
                 together with the missing member's Linux row.\n",
            );
        }
        if phase == Phase::Published || (self.ready.is_empty() && self.signed_earlier.is_empty()) {
            return s;
        }
        s.push_str(&format!(
            "\nThe ceremony, from THIS directory on a rostered machine (do not reformat any \
             staged file — the signature covers exact bytes; `sign` uses the machine key at \
             {key}, `upload staging` the staging token and `upload public` the channel token \
             at {token}):\n\n```sh\n"
        ));
        for (p, new) in self.ready {
            let (name, build) = (&p.name, p.build);
            let dir = Path::new(name);
            let manifest = rows::local(&staged_manifest(self.staged_dir, name, build)).ok();
            let tarballs: String = new
                .iter()
                .filter_map(|t| manifest.as_ref()?.artifact_for(t.as_str()))
                .map(|a| format!(" {}", dir.join(&a.asset).display()))
                .collect();
            let pkg = dir.join(pkgdoc::manifest_name(name, build));
            let sig = dir.join(pkgdoc::sig_name(name, build));
            let tag = upload::tag(name, build);
            s.push_str(&format!(
                "# --- {name}@{build} ({}) ---\n\
                 atpkg-publish sign {}\n\
                 atpkg-publish upload staging {} {tag}{tarballs} {} {}\n\
                 atpkg-publish upload public {} {tag}{tarballs} {} {}\n",
                new.iter()
                    .map(|t| t.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                pkg.display(),
                p.repo,
                pkg.display(),
                sig.display(),
                p.repo,
                pkg.display(),
                sig.display(),
            ));
        }
        if !self.ready.is_empty() {
            s.push('\n');
        }
        s.push_str(&format!(
            "# --- follow-up index {next} (pins unchanged; index_build ratchets so clients \
             re-plan) ---\natpkg-publish index --upload\n```\n"
        ));
        if !self.ready.is_empty() {
            s.push_str(
                "\nThe public uploads above are what keep the indexer's pack mirror green: both\n\
                 owners must carry the SAME replaced manifest bytes, or it refuses the pack.\n",
            );
        }
        s
    }
}
