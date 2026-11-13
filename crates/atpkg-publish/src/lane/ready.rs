// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The Apple ready queue: immutable records of qualified, signed, staged builds, and the
//! staged-only dispatcher that publishes them in one index.
//!
//! A record is the explicit qualified transition: a pack on the staging owner publishes nothing
//! until a record names it, and the record binds the exact signed manifest that was qualified.
//! The dispatcher starts no build. Under its own lock it reads the fresh signed head, holds each
//! record to that head and to the lifecycle registry, and advances exactly the recorded pins,
//! each as its own target's pin (`pin_by_target`): the channel's `pin`, every other target's
//! selection and everything else in the head are carried as they are.
//!
//! No record waits behind another, and none waits for ever. A publish the publisher refuses
//! with the head where it was is retried with each of its records alone, so the others are
//! pinned; a record refused [`SET_ASIDE_AFTER`] times is set aside and its build latched. A
//! record whose staged bytes are no longer the qualified ones (re-staged by hand, deleted, no
//! longer verifying) is set aside at once, and the tip lane qualifies that build again. A
//! record of a build the head yanked or floored out is set aside for good. Only a hold that a
//! decision elsewhere lifts (the lifecycle registry, a first pin, another target's machine)
//! leaves its record queued, and its line says what lifts it and how to drop it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::LaneKind;
use super::lifecycle::Registry;
use super::report::{self, NoticeSink, Transcript};
use super::rows::{self, Fetched};
use super::state::{LaneState, Lock};
use crate::api::{Baseline, Build, Delta, IndexRequest, Indexer};
use crate::artifact::qualify;
use crate::artifact::upload;
use crate::ctx::Ctx;
use crate::error::{Error, Result};
use crate::host::Slug;
use crate::programs::{self, ProgramDef};
use crate::quad::{self, VerifiedQuad};
use crate::triple::Triple;

/// The transcript's prefix.
pub const ME: &str = "auto-tip-ready";

/// The record format.
pub const SCHEMA: &str = "atpkg.lane.ready.v1";

/// A record's file name ends with this.
pub const SUFFIX: &str = ".ready.toml";

/// The largest record read.
pub const MAX_RECORD_BYTES: u64 = 1024 * 1024;

/// Index publishes one round attempts: the first, and one re-plan over each fresher head another
/// publisher's index left.
pub const PUBLISH_ATTEMPTS: u32 = 3;

/// Times the publisher may refuse one queued build, or the staging owner may be found holding
/// other bytes than the ones qualified for it, before the build is set aside and latched.
pub const SET_ASIDE_AFTER: u32 = 3;

/// One qualification step, as a record keeps it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StepRecord {
    /// The step.
    pub name: String,
    /// `source` | `build` | `packed`.
    pub stage: String,
    /// The target whose packed tree it ran on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Its exit code.
    pub exit: i64,
    /// How long it took.
    pub millis: u64,
    /// Its committed bound.
    pub within_secs: u64,
    /// SHA-256 of its kept stdout.
    pub stdout_sha256: String,
    /// SHA-256 of its kept stderr.
    pub stderr_sha256: String,
    /// libtest tests that ran, when it printed a summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tests_ran: Option<u64>,
}

impl StepRecord {
    /// A passed step's record. `None` for one that did not pass or left no exit: a record
    /// carries only what qualified.
    #[must_use]
    pub fn of(receipt: &qualify::Receipt) -> Option<Self> {
        if !receipt.passed() {
            return None;
        }
        Some(Self {
            name: receipt.name.clone(),
            stage: receipt.stage.as_str().to_string(),
            target: receipt.target.map(|t| t.as_str().to_string()),
            exit: i64::from(receipt.exit?),
            millis: receipt.millis,
            within_secs: receipt.within_secs,
            stdout_sha256: receipt.stdout_sha256.clone(),
            stderr_sha256: receipt.stderr_sha256.clone(),
            tests_ran: receipt.tests.map(qualify::Tests::ran),
        })
    }
}

/// One qualified, signed, staged build that waits for an index.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Record {
    /// [`SCHEMA`].
    pub schema: String,
    /// The program.
    pub program: String,
    /// Its repository.
    pub repo: String,
    /// The frozen commit it was built from (40 lowercase hex).
    pub source: String,
    /// Its build number: that commit's count.
    pub build: Build,
    /// The version its manifest signs.
    pub version: String,
    /// The target it was packed and controlled for: the one target whose pin it asks for.
    pub target: String,
    /// The targets its manifest carries.
    pub targets: Vec<String>,
    /// SHA-256 of the signed manifest bytes that were qualified and staged.
    pub manifest_sha256: String,
    /// The pin it was qualified above.
    pub pin_before: Build,
    /// The index it was decided over.
    pub decided_over_index: u64,
    /// When it qualified.
    pub qualified_unix: i64,
    /// What qualified it, in order.
    pub steps: Vec<StepRecord>,
}

impl Record {
    /// Its file name: `<program>-<build>.ready.toml`.
    #[must_use]
    pub fn file_name(&self) -> String {
        format!("{}-{}{SUFFIX}", self.program, self.build)
    }

    /// The record's own consistency: the schema, a 40-hex source, a build above the pin it was
    /// qualified over, a 64-hex manifest digest, a target its manifest carries, and at least
    /// one build and one packed step that exited 0.
    /// # Errors
    /// The first thing wrong.
    pub fn check(&self) -> std::result::Result<(), String> {
        let hex = |s: &str, n: usize| {
            s.len() == n && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        };
        if self.schema != SCHEMA {
            return Err(format!("schema `{}` is not {SCHEMA}", self.schema));
        }
        if !hex(&self.source, 40) {
            return Err("its source is not a full lowercase commit".into());
        }
        if !hex(&self.manifest_sha256, 64) {
            return Err("its manifest digest is not a SHA-256".into());
        }
        if self.build <= self.pin_before {
            return Err(format!(
                "build {} is not above the pin {} it was qualified over",
                self.build, self.pin_before
            ));
        }
        if Triple::parse(&self.target).is_err()
            || self.targets.iter().any(|t| Triple::parse(t).is_err())
        {
            return Err("it names no client target".into());
        }
        if !self.targets.contains(&self.target) {
            return Err(format!(
                "its manifest carries no {} row, the target it asks to pin",
                self.target
            ));
        }
        if self.steps.iter().any(|s| s.exit != 0) {
            return Err("it carries a step that did not exit 0".into());
        }
        for stage in ["build", "packed"] {
            if !self.steps.iter().any(|s| s.stage == stage) {
                return Err(format!("it carries no {stage} step"));
            }
        }
        Ok(())
    }
}

/// Where `state`'s records are kept: the lanes' ready directory, which the installed dispatcher
/// watches.
#[must_use]
pub fn dir(state: &LaneState) -> PathBuf {
    state.ready_dir()
}

/// Enqueue `record`: write it once, atomically, into the private ready directory. A record is
/// immutable: the same record again changes nothing, and a different one for the same build is
/// refused.
/// # Errors
/// An inconsistent record, an unsafe directory, a different record already there, or a failed
/// write.
pub fn enqueue(state: &LaneState, record: &Record) -> Result<PathBuf> {
    record
        .check()
        .map_err(|why| Error::refused(format!("not a ready record: {why}")))?;
    let text = aterm_toml::to_string(record)
        .map_err(|e| Error::refused(format!("encode the ready record: {e}")))?;
    let directory = dir(state);
    aterm_types::fs_restricted::ensure_private_dir(&directory)
        .map_err(|e| Error::io("create the ready directory", &e))?;
    let path = directory.join(record.file_name());
    match crate::identity::read_regular_bytes(&path, MAX_RECORD_BYTES) {
        Ok(existing) if existing == text.as_bytes() => return Ok(path),
        Ok(_) => {
            return Err(Error::refused(format!(
                "{} already holds a different record for {} build {} — a ready record is \
                 immutable; a new qualification is a new build",
                path.display(),
                record.program,
                record.build
            )));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(Error::io(format!("read {}", path.display()), &e)),
    }
    aterm_uds::atomic_file::write(&path, text.as_bytes(), 0o600)
        .map_err(|e| Error::io(format!("write {}", path.display()), &e))?;
    Ok(path)
}

/// One file of the ready directory.
#[derive(Clone, Debug)]
pub struct Queued {
    /// The file.
    pub path: PathBuf,
    /// Its record, or why it is not one.
    pub record: std::result::Result<Record, String>,
}

/// Every record waiting in `state`'s ready directory, by file name. A missing directory is an
/// empty queue.
/// # Errors
/// A ready directory that is not a real directory, or cannot be listed.
pub fn pending(state: &LaneState) -> Result<Vec<Queued>> {
    let directory = dir(state);
    match std::fs::symlink_metadata(&directory) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
        Ok(_) => return Err(Error::refused("unsafe ready directory")),
        Err(e) => return Err(Error::io("inspect the ready directory", &e)),
    }
    let mut names: Vec<String> = std::fs::read_dir(&directory)
        .map_err(|e| Error::io("list the ready directory", &e))?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(SUFFIX))
        .collect();
    names.sort();
    Ok(names
        .into_iter()
        .map(|name| {
            let path = directory.join(&name);
            let record = crate::identity::read_regular_text(&path, MAX_RECORD_BYTES)
                .map_err(|e| format!("cannot be read: {e}"))
                .and_then(|text| {
                    aterm_toml::from_str::<Record>(&text)
                        .map_err(|e| format!("does not parse: {e}"))
                })
                .and_then(|record| {
                    record.check()?;
                    if record.file_name() == name {
                        Ok(record)
                    } else {
                        Err(format!(
                            "names {} build {}, not what its file name says",
                            record.program, record.build
                        ))
                    }
                });
            Queued { path, record }
        })
        .collect())
}

/// A program the Apple ready queue may carry: a plain row of its own repository that moves
/// alone.
/// # Errors
/// Why `name` is not one.
pub fn queueable(name: &str) -> std::result::Result<&'static ProgramDef, String> {
    let def = programs::get(name).ok_or_else(|| format!("{name} is not a program"))?;
    if !def.is_plain() {
        return Err(format!(
            "{name} is not a plain program: its pack is not the lane's"
        ));
    }
    if let Some(group) = def.group {
        return Err(format!(
            "{name} moves with coherence group `{group}`: one seal cuts every member, through \
             publish-group"
        ));
    }
    if def.repo != def.name {
        return Err(format!(
            "{name} is built from {}'s repository: it ships with that product's own release",
            def.repo
        ));
    }
    Ok(def)
}

/// Move a record out of the queue into `<lane dir>/retired/`, keeping it as the receipt of what
/// became of it (`why` is its new suffix: `published`, `covered`, `superseded`, `invalid`,
/// `restaged`, `withdrawn`, `refused`).
fn retire(state: &LaneState, path: &Path, why: &str) -> Result<()> {
    let keep = state.dir.join("retired");
    std::fs::create_dir_all(&keep)
        .map_err(|e| Error::io(format!("mkdir {}", keep.display()), &e))?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    std::fs::rename(path, keep.join(format!("{name}.{why}")))
        .map_err(|e| Error::io(format!("retire {}", path.display()), &e))
}

/// How one pass is told to behave.
#[derive(Clone, Debug)]
pub struct Options {
    /// Decide and print; publish, retire and record nothing.
    pub dry_run: bool,
    /// Started by the ready directory's watcher: an empty queue is idle without contacting a
    /// host, and a queue this pass leaves unpublished is a refusal.
    pub queued: bool,
    /// The publication engine's repository, whose main holds the lifecycle registry.
    pub engine: Slug,
    /// The target this machine's records are for: the one target whose pins a pass moves.
    pub host: Triple,
}

impl Options {
    /// The production defaults for `ctx` on a machine that runs `host`.
    #[must_use]
    pub fn new(ctx: &Ctx, dry_run: bool, queued: bool, host: Triple) -> Self {
        Self {
            dry_run,
            queued,
            engine: ctx.staging_slug(super::alab::ENGINE_REPO),
            host,
        }
    }
}

/// What the dispatcher reaches beyond the context.
pub struct Deps<'a> {
    /// Publishes the index.
    pub indexer: &'a dyn Indexer,
    /// Appends notices to the journal.
    pub notifier: &'a dyn NoticeSink,
}

/// What one pass did.
#[derive(Clone, Debug, Default)]
pub struct Outcome {
    /// Another pass held the lock.
    pub covered: bool,
    /// The queue was empty, or nothing in it was left to publish.
    pub idle: bool,
    /// The verified head it decided over (the last one, when it re-planned).
    pub head: Option<u64>,
    /// The index it published (the last one, when records were published alone).
    pub published: Option<u64>,
    /// `<program>=<build>` it pinned for its target (or, in a dry run, would pin).
    pub ready: Vec<String>,
    /// `<program>=<build>: why` it could not judge this pass (an unreachable host).
    pub deferred: Vec<String>,
    /// `<program>=<build>: why` it will not publish as things stand, the publisher's refusals
    /// of this pass included.
    pub held: Vec<String>,
    /// Records it retired, and as what.
    pub retired: Vec<String>,
}

enum Verdict {
    Ready,
    Covered,
    Deferred(String),
    /// It cannot publish as things stand, and stays queued until what the words name changes.
    Held(String),
    /// The staging owner no longer holds the bytes that were qualified: the record binds
    /// nothing, and its build is the tip lane's to qualify again.
    Restaged(String),
    /// The head withdrew this build (a yank, or a floor above it): it is never pinned.
    Withdrawn(String),
}

/// Why the head's channel, as one target sees it, will not have `program` at `build`: the build
/// is yanked or below the program's floor, and a client holding such a pin tombstones the
/// program. `None` when the build may be pinned.
#[must_use]
pub fn withdrawn(
    channel: &atpkg::manifest::Channel,
    program: &str,
    build: Build,
) -> Option<String> {
    if atpkg::gate::is_yanked(channel, program, build) {
        return Some(format!(
            "the head yanks {program}@{build}: a withdrawn build is never pinned again"
        ));
    }
    let floor = channel.min_build_for(program);
    (build < floor)
        .then(|| format!("{program} build {build} is below the head's floor of {floor} for it"))
}

#[allow(clippy::too_many_arguments)]
fn judge(
    ctx: &Ctx,
    host: Triple,
    head: &VerifiedQuad,
    view: &atpkg::manifest::Channel,
    pins: &BTreeMap<String, Build>,
    registry: &Registry,
    work: &Path,
    record: &Record,
) -> Verdict {
    let def = match queueable(&record.program) {
        Ok(def) if def.repo == record.repo => def,
        Ok(def) => {
            return Verdict::Held(format!(
                "the record names repository {}, and {} is built from {}",
                record.repo, record.program, def.repo
            ));
        }
        Err(why) => return Verdict::Held(why),
    };
    if record.target != host.as_str() {
        return Verdict::Held(format!(
            "the record is for {}, and this machine publishes {host} pins: a machine that runs \
             {} publishes it",
            record.target, record.target
        ));
    }
    if let Some(why) = registry.standing(def.repo).refusal(def.repo) {
        return Verdict::Held(format!(
            "{why}: it publishes once the lifecycle registry holds {} active with an open \
             public tier",
            def.repo
        ));
    }
    let Some(pin) = pins.get(def.name).copied() else {
        return Verdict::Held(format!(
            "{} is not pinned by the head: a new program is pinned by hand, never by a lane \
             (`atpkg-publish index --pin {}={}`)",
            def.name, def.name, record.build
        ));
    };
    if record.build <= pin {
        return Verdict::Covered;
    }
    if let Some(why) = withdrawn(view, def.name, record.build) {
        return Verdict::Withdrawn(why);
    }
    let tag = upload::tag(def.name, record.build);
    match rows::fetch(
        &ctx.hosts.staging,
        &ctx.staging_slug(def.repo),
        def.name,
        record.build,
        &work.join("staged").join(def.name),
        head,
    ) {
        Fetched::Present { manifest, path } => {
            let digest = match atpkg::tree::sha256_file(&path) {
                Ok(digest) => digest,
                Err(e) => return Verdict::Deferred(format!("hash the staged manifest: {e}")),
            };
            if digest != record.manifest_sha256 {
                return Verdict::Restaged(format!(
                    "{tag} on {} is not the manifest that was qualified (sha256 {digest}, the \
                     record binds {})",
                    ctx.accounts.staging, record.manifest_sha256
                ));
            }
            if rows::targets(&manifest).contains(&host) {
                Verdict::Ready
            } else {
                Verdict::Held(format!("{tag} carries no {host} row to pin"))
            }
        }
        Fetched::Absent(why) => {
            Verdict::Restaged(format!("{tag} is not on {} ({why})", ctx.accounts.staging))
        }
        Fetched::Unreachable(why) => Verdict::Deferred(format!("{tag} could not be read ({why})")),
        Fetched::Invalid(why) => Verdict::Restaged(format!("{tag} does not verify ({why})")),
    }
}

/// One record a pass sets aside, and why.
struct Aside {
    path: PathBuf,
    program: String,
    build: Build,
    why: String,
}

struct Plan {
    n: u64,
    head: VerifiedQuad,
    pins: BTreeMap<String, Build>,
    /// program → (build, the record's file).
    ready: BTreeMap<String, (Build, PathBuf)>,
    covered: Vec<PathBuf>,
    superseded: Vec<PathBuf>,
    invalid: Vec<(PathBuf, String)>,
    restaged: Vec<Aside>,
    withdrawn: Vec<Aside>,
    deferred: Vec<String>,
    held: Vec<String>,
}

fn plan(
    ctx: &Ctx,
    opts: &Options,
    state: &LaneState,
    queue: &[Queued],
    work: &Path,
    tx: &Transcript,
) -> Result<Plan> {
    let slug = ctx.index_slug();
    let (n, head) = quad::fetch_head(ctx, &slug, &work.join("head")).map_err(|e| match e {
        Error::CannotRun(why) => Error::cannot_run(format!(
            "could not read an atpkg-index-N release on {slug} ({why}) — the ready records \
             stay queued"
        )),
        other => Error::refused(format!(
            "the channel head on {slug} is not a baseline this dispatcher will build on — \
             {other}"
        )),
    })?;
    state.check_index(n)?;
    let channel = head.index.channels.first().cloned().ok_or_else(|| {
        Error::refused(format!(
            "index-{n} carries no [[channels]] block — no pins to advance"
        ))
    })?;
    // This target's view of the channel: `pin` with its own `pin_by_target` overlay laid over.
    let view = channel.for_target(opts.host.as_str());
    let pins = crate::pins::for_targets(&channel, &[opts.host])?;
    state.check_pins(&[opts.host], &pins, n, tx)?;
    let (at, registry) = super::lifecycle::read(ctx, &opts.engine, work).map_err(|why| {
        Error::cannot_run(format!(
            "the lifecycle registry could not be read ({why}) — nothing is published without it"
        ))
    })?;
    tx.say(format!(
        "channel index head: atpkg-index-{n}, as {} sees it; lifecycle registry: {at}; {} \
         record(s) judged",
        opts.host,
        queue.len()
    ));
    let mut plan = Plan {
        n,
        head,
        pins,
        ready: BTreeMap::new(),
        covered: Vec::new(),
        superseded: Vec::new(),
        invalid: Vec::new(),
        restaged: Vec::new(),
        withdrawn: Vec::new(),
        deferred: Vec::new(),
        held: Vec::new(),
    };
    for queued in queue {
        let record = match &queued.record {
            Ok(record) => record,
            Err(why) => {
                plan.invalid.push((queued.path.clone(), why.clone()));
                continue;
            }
        };
        let label = format!("{}={}", record.program, record.build);
        let aside = |why: String| Aside {
            path: queued.path.clone(),
            program: record.program.clone(),
            build: record.build,
            why,
        };
        match judge(
            ctx, opts.host, &plan.head, &view, &plan.pins, &registry, work, record,
        ) {
            Verdict::Covered => plan.covered.push(queued.path.clone()),
            Verdict::Deferred(why) => plan.deferred.push(format!("{label}: {why}")),
            // A hold stands until what it names changes, and says how to drop the record.
            Verdict::Held(why) => plan.held.push(format!(
                "{label}: {why} — it stays queued until then; to drop it, remove {}",
                ctx.shown(&queued.path)
            )),
            Verdict::Restaged(why) => plan.restaged.push(aside(why)),
            Verdict::Withdrawn(why) => plan.withdrawn.push(aside(why)),
            Verdict::Ready => {
                // Two qualified builds of one program: the newer is pinned, and the older is
                // behind it the moment it is.
                match plan.ready.get(&record.program) {
                    Some((build, _)) if *build >= record.build => {
                        plan.superseded.push(queued.path.clone());
                    }
                    earlier => {
                        if let Some((_, path)) = earlier {
                            plan.superseded.push(path.clone());
                        }
                        plan.ready
                            .insert(record.program.clone(), (record.build, queued.path.clone()));
                    }
                }
            }
        }
    }
    Ok(plan)
}

fn labels(ready: &BTreeMap<String, (Build, PathBuf)>) -> Vec<String> {
    ready
        .iter()
        .map(|(program, (build, _))| format!("{program}={build}"))
        .collect()
}

fn list(items: &[String]) -> String {
    if items.is_empty() {
        "<none>".into()
    } else {
        items.join("; ")
    }
}

/// One pass of the Apple ready dispatcher.
///
/// # Errors
/// A refusal for a downgrade, a head that does not verify, an index that did not publish, or
/// (queued) records this pass left unpublished; a could-not-run when the head or the lifecycle
/// registry cannot be read or this machine cannot sign.
pub fn dispatch(ctx: &Ctx, opts: &Options, deps: &Deps<'_>, tx: &Transcript) -> Result<Outcome> {
    let state = LaneState::of(LaneKind::Tip, &ctx.state_dir);
    if opts.dry_run {
        // No lock, no state: a dry run reads into a private scratch directory it removes.
        let queue = pending(&state)?;
        if queue.is_empty() {
            tx.say("no ready record is queued — nothing to publish");
            return Ok(Outcome {
                idle: true,
                ..Outcome::default()
            });
        }
        let scratch = crate::channel::Scratch::new("tip-ready-dry-run")?;
        let plan = plan(ctx, opts, &state, &queue, scratch.path(), tx)?;
        let ready = labels(&plan.ready);
        let requalify: Vec<String> = plan
            .restaged
            .iter()
            .chain(&plan.withdrawn)
            .map(|a| format!("{}={}: {}", a.program, a.build, a.why))
            .collect();
        tx.say(format!(
            "DRY_RUN — would publish atpkg-index-{} pinning for {}: {}; held: {}; deferred: \
             {}; would set aside: {}; nothing published, retired or recorded",
            plan.n.saturating_add(1),
            opts.host,
            list(&ready),
            list(&plan.held),
            list(&plan.deferred),
            list(&requalify)
        ));
        return Ok(Outcome {
            idle: ready.is_empty() && plan.held.is_empty() && plan.deferred.is_empty(),
            head: Some(plan.n),
            ready,
            deferred: plan.deferred,
            held: plan.held,
            ..Outcome::default()
        });
    }
    state.ensure()?;
    let Lock::Held(_guard) = state.queued_lock()? else {
        tx.say(format!(
            "another pass holds {} — it covers this one; exiting 0",
            state.publish_lock.display()
        ));
        return Ok(Outcome {
            covered: true,
            ..Outcome::default()
        });
    };
    let queue = pending(&state)?;
    if queue.is_empty() {
        if !opts.queued {
            tx.say("no ready record is queued — nothing to publish");
        }
        return Ok(Outcome {
            idle: true,
            ..Outcome::default()
        });
    }
    let result = locked(ctx, opts, deps, &state, tx);
    if let Err(e) = &result {
        report::record_once(
            deps.notifier,
            super::tip::TITLE,
            &state.dir,
            ctx.now_unix,
            false,
            &format!("ready dispatch FAILED: {e}"),
        );
    }
    result
}

fn preflight(ctx: &Ctx, head: &VerifiedQuad) -> Result<()> {
    for (what, path) in [
        ("machine key", &ctx.identity.key),
        ("machine record", &ctx.identity.record),
    ] {
        if !path.is_file() {
            return Err(Error::cannot_run(format!(
                "no {what} at {} — the dispatcher SIGNS the index; provision this Mac \
                 (`atpkg-keys join --id <id>`) or run it on a rostered one",
                ctx.shown(path)
            )));
        }
    }
    crate::identity::signer_for(ctx, &head.roster)?;
    crate::artifact::sign::elect_from_head(ctx, head);
    Ok(())
}

/// Which queued records one publish may carry.
enum Only<'a> {
    /// Every record but those of these programs (and every file that is not a record).
    Except(&'a BTreeSet<String>),
    /// The records of this one program.
    Program(&'a str),
}

impl Only<'_> {
    fn admits(&self, queued: &Queued) -> bool {
        match (self, &queued.record) {
            (Self::Except(_), Err(_)) => true,
            (Self::Except(programs), Ok(record)) => !programs.contains(&record.program),
            (Self::Program(_), Err(_)) => false,
            (Self::Program(program), Ok(record)) => record.program == *program,
        }
    }
}

/// How one publish of a set of records ended.
enum Round {
    /// Nothing in the set was ready.
    Nothing,
    /// One index pins every ready record of the set.
    Published,
    /// The publisher refused the index with the head where it was: the refusal is about these
    /// records (program → build and file), not about a race.
    Refused(BTreeMap<String, (Build, PathBuf)>, Error),
}

/// What a locked pass works with.
struct Desk<'a> {
    ctx: &'a Ctx,
    opts: &'a Options,
    deps: &'a Deps<'a>,
    state: &'a LaneState,
    work: PathBuf,
    tx: &'a Transcript,
}

impl Desk<'_> {
    fn notice(&self, message: &str) {
        report::record_once(
            self.deps.notifier,
            super::tip::TITLE,
            &self.state.dir,
            self.ctx.now_unix,
            false,
            message,
        );
    }

    /// Publish the ready records `only` admits in one index over the fresh head, re-planning
    /// over each fresher head another publisher's index leaves.
    fn round(&self, only: &Only<'_>, outcome: &mut Outcome) -> Result<Round> {
        let (ctx, opts, state, tx) = (self.ctx, self.opts, self.state, self.tx);
        let (held, deferred) = (outcome.held.len(), outcome.deferred.len());
        let mut attempt = 0;
        loop {
            attempt += 1;
            // What an earlier attempt of this round held is judged again over the fresh head.
            outcome.held.truncate(held);
            outcome.deferred.truncate(deferred);
            let queue: Vec<Queued> = pending(state)?
                .into_iter()
                .filter(|queued| only.admits(queued))
                .collect();
            if queue.is_empty() {
                return Ok(Round::Nothing);
            }
            let _ = std::fs::remove_dir_all(&self.work);
            std::fs::create_dir_all(&self.work)
                .map_err(|e| Error::io(format!("mkdir {}", self.work.display()), &e))?;
            let plan = plan(ctx, opts, state, &queue, &self.work, tx)?;
            outcome.head = Some(plan.n);
            for line in plan.held.iter().chain(&plan.deferred) {
                tx.warn(format!("not published: {line}"));
            }
            for (lines, into) in [
                (&plan.held, &mut outcome.held),
                (&plan.deferred, &mut outcome.deferred),
            ] {
                for line in lines {
                    if !into.contains(line) {
                        into.push(line.clone());
                    }
                }
            }
            if plan.ready.is_empty() {
                settle(state, &plan, None, outcome, tx)?;
                state.raise(plan.n, &[opts.host], &plan.pins)?;
                return Ok(Round::Nothing);
            }
            preflight(ctx, &plan.head)?;
            // A target pin for each record: the channel's `pin` and every other target's
            // overlay are not in the delta, so the index carries them as the head has them.
            let mut delta = Delta::default();
            let mut moved = plan.pins.clone();
            for (program, (build, _)) in &plan.ready {
                delta
                    .target_pins
                    .entry(opts.host)
                    .or_default()
                    .insert(program.clone(), *build);
                moved.insert(program.clone(), *build);
            }
            let ready = labels(&plan.ready);
            tx.say(format!(
                "publishing atpkg-index-{} (baseline index-{}) pinning for {}: {}",
                plan.n.saturating_add(1),
                plan.n,
                opts.host,
                list(&ready)
            ));
            let published = self.deps.indexer.publish(
                ctx,
                &IndexRequest {
                    baseline: Baseline::Dir(plan.head.dir.clone()),
                    delta,
                    out: state.dir.join("index-out"),
                    upload: true,
                    pack_dir: None,
                },
            );
            match published {
                Ok(published) => {
                    state.raise(published.index_build, &[opts.host], &moved)?;
                    outcome.published = Some(published.index_build);
                    settle(state, &plan, Some(published.index_build), outcome, tx)?;
                    for (program, (build, _)) in &plan.ready {
                        let _ = std::fs::remove_file(state.publish_attempts_file(program, *build));
                    }
                    let summary = format!(
                        "published atpkg-index-{}: {}",
                        published.index_build,
                        list(&ready)
                    );
                    outcome.ready.extend(ready);
                    tx.say(&summary);
                    self.notice(&summary);
                    return Ok(Round::Published);
                }
                // The index is committed; only its readback is open. The next pass finds the
                // pins moved and retires the records: it is never published twice.
                Err(e @ Error::PublishedUnconfirmed { .. }) => return Err(e),
                Err(e) => {
                    // Another publisher's index is the one refusal a pure pin advance re-plans
                    // over: it is told by the head having moved, never by the refusal's wording.
                    let moved_to = quad::head_build(ctx, &ctx.index_slug()).ok().flatten();
                    let next = plan.n.saturating_add(1);
                    if moved_to.is_some_and(|now| now > plan.n) {
                        if attempt < PUBLISH_ATTEMPTS {
                            tx.warn(format!(
                                "index {next} did not publish ({e}); the head moved to index-{} \
                                 — re-planning over it (attempt {} of {PUBLISH_ATTEMPTS})",
                                moved_to.unwrap_or(plan.n),
                                attempt + 1
                            ));
                            continue;
                        }
                        return Err(Error::refused(format!(
                            "index {next} did not publish ({e}): other publishers moved the \
                             head {PUBLISH_ATTEMPTS} times — the ready records stay queued for \
                             the next pass"
                        )));
                    }
                    // A publish that could not run says nothing about its records.
                    if let Error::CannotRun(why) = e {
                        return Err(Error::cannot_run(format!(
                            "index {next} did not publish ({why}) — the ready records stay \
                             queued for the next pass"
                        )));
                    }
                    settle(state, &plan, None, outcome, tx)?;
                    return Ok(Round::Refused(plan.ready, e));
                }
            }
        }
    }

    /// Count the publisher's refusal of one record. Below [`SET_ASIDE_AFTER`] it stays queued,
    /// and is published alone from now on; at the bound it is set aside and its build latched,
    /// so neither the dispatcher nor the tip lane spends another pass on it.
    fn strike(&self, program: &str, build: Build, path: &Path, why: &Error) -> Result<String> {
        let (state, tx) = (self.state, self.tx);
        let n = state.bump_publish_attempts(program, build);
        let line = if n < SET_ASIDE_AFTER {
            format!(
                "{program}={build}: the publisher refused it ({why}) — refusal {n} of \
                 {SET_ASIDE_AFTER}; it stays queued and is published alone"
            )
        } else {
            retire(state, path, "refused")?;
            latch(
                state,
                program,
                build,
                n,
                &format!("the publisher refused it: {why}"),
            );
            format!(
                "{program}={build}: the publisher refused it {n} times ({why}) — set aside as \
                 retired/{}.refused and latched by {}: fix what the refusal names, then delete \
                 the latch to requalify the build; a newer main retries by itself",
                file_name(path),
                self.ctx.shown(&state.blocked_file(program, build))
            )
        };
        tx.warn(format!("not published: {line}"));
        self.notice(&line);
        Ok(line)
    }
}

/// Write the tip lane's latch for `program` `build`: while it exists the tip lane does not
/// qualify that build again.
fn latch(state: &LaneState, program: &str, build: Build, n: u32, headline: &str) {
    let text = format!(
        "# auto-tip: {program} build {build} set aside by the ready dispatcher after {n} \
         attempts\n\n{headline}\n\n- this file is the latch: the tip lane does not qualify \
         {program} build {build} again while it exists, and the dispatcher holds no record of \
         it (the last one is under retired/).\n  Fix what the words above name, then delete \
         this file (and publish-attempts-{program}-{build}): the next tip pass qualifies, \
         stages and queues the build again. A newer main retries by itself.\n"
    );
    let _ = report::write_atomic(&state.blocked_file(program, build), &text);
}

fn locked(
    ctx: &Ctx,
    opts: &Options,
    deps: &Deps<'_>,
    state: &LaneState,
    tx: &Transcript,
) -> Result<Outcome> {
    tx.raw(format!(
        "=== {ME} {} pid {} ===",
        aterm_types::rfc3339::format_rfc3339(u64::try_from(ctx.now_unix).unwrap_or(0)),
        std::process::id()
    ));
    let desk = Desk {
        ctx,
        opts,
        deps,
        state,
        work: state.dir.join("ready-work"),
        tx,
    };
    let mut outcome = Outcome::default();
    // A record the publisher already refused is published alone: one index for every other
    // record first, so a record the publisher will not take never holds the rest.
    let suspects: BTreeSet<String> = pending(state)?
        .iter()
        .filter_map(|queued| queued.record.as_ref().ok())
        .filter(|record| state.publish_attempts(&record.program, record.build) > 0)
        .map(|record| record.program.clone())
        .collect();
    let mut alone: Vec<String> = Vec::new();
    let mut refused: Vec<String> = Vec::new();
    match desk.round(&Only::Except(&suspects), &mut outcome)? {
        Round::Nothing | Round::Published => {}
        Round::Refused(ready, why) if ready.len() == 1 => {
            for (program, (build, path)) in &ready {
                refused.push(desk.strike(program, *build, path, &why)?);
            }
        }
        Round::Refused(ready, why) => {
            tx.warn(format!(
                "index {} did not publish ({why}) with the head where it was — publishing each \
                 of its {} records alone, so the one it refuses does not hold the others",
                outcome.head.unwrap_or(0).saturating_add(1),
                ready.len()
            ));
            alone.extend(ready.into_keys());
        }
    }
    alone.extend(suspects);
    for program in &alone {
        if let Round::Refused(ready, why) = desk.round(&Only::Program(program), &mut outcome)? {
            for (program, (build, path)) in &ready {
                refused.push(desk.strike(program, *build, path, &why)?);
            }
        }
    }
    outcome.held.extend(refused.iter().cloned());
    if outcome.published.is_some() {
        return Ok(outcome);
    }
    if !refused.is_empty() {
        return Err(Error::refused(format!(
            "index {} did not publish — {}",
            outcome.head.unwrap_or(0).saturating_add(1),
            refused.join("; ")
        )));
    }
    if outcome.held.is_empty() && outcome.deferred.is_empty() {
        tx.say("nothing queued is left to publish — idle");
        outcome.idle = true;
        return Ok(outcome);
    }
    if opts.queued {
        Err(Error::refused(format!(
            "queued ready records were not published and stay queued — held: {}; deferred: {}",
            list(&outcome.held),
            list(&outcome.deferred)
        )))
    } else {
        Ok(outcome)
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Retire what this pass settled: records the head already covers, records a newer build of the
/// same program replaced, the records it published, files that are not records, records of a
/// build the head withdrew, and records whose staged bytes are no longer the qualified ones.
/// The last are the tip lane's to qualify again, at most [`SET_ASIDE_AFTER`] times per build.
fn settle(
    state: &LaneState,
    plan: &Plan,
    published: Option<u64>,
    outcome: &mut Outcome,
    tx: &Transcript,
) -> Result<()> {
    for path in &plan.covered {
        retire(state, path, "covered")?;
        outcome.retired.push(format!(
            "{}: the head already pins it or a newer build",
            file_name(path)
        ));
    }
    for (path, why) in &plan.invalid {
        retire(state, path, "invalid")?;
        tx.warn(format!(
            "{} is not a ready record ({why}) — set aside",
            file_name(path)
        ));
        outcome
            .retired
            .push(format!("{}: invalid ({why})", file_name(path)));
    }
    for aside in &plan.withdrawn {
        retire(state, &aside.path, "withdrawn")?;
        tx.warn(format!(
            "{} is set aside: {}",
            file_name(&aside.path),
            aside.why
        ));
        outcome.retired.push(format!(
            "{}: withdrawn ({})",
            file_name(&aside.path),
            aside.why
        ));
    }
    for aside in &plan.restaged {
        retire(state, &aside.path, "restaged")?;
        let n = state.bump_publish_attempts(&aside.program, aside.build);
        let next = if n < SET_ASIDE_AFTER {
            "the next tip pass qualifies and stages the build again".to_string()
        } else {
            latch(state, &aside.program, aside.build, n, &aside.why);
            format!(
                "re-staged {n} times, so the build is latched by {}",
                file_name(&state.blocked_file(&aside.program, aside.build))
            )
        };
        tx.warn(format!(
            "{} no longer names what is staged ({}) — set aside; {next}",
            file_name(&aside.path),
            aside.why
        ));
        outcome.retired.push(format!(
            "{}: restaged ({}); {next}",
            file_name(&aside.path),
            aside.why
        ));
    }
    if let Some(index) = published {
        for (_, path) in plan.ready.values() {
            retire(state, path, "published")?;
            outcome
                .retired
                .push(format!("{}: published in index {index}", file_name(path)));
        }
        for path in &plan.superseded {
            retire(state, path, "superseded")?;
            outcome
                .retired
                .push(format!("{}: a newer build was pinned", file_name(path)));
        }
    }
    Ok(())
}
