// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The tip lane: keep a qualified, signed, staged build of each plain program's dev main ready
//! for the index.
//!
//! A pass serves one target, the machine's own. It compares main with that target's pin
//! (`Channel::for_target`), packs that target's row and runs the packed controls on it, and its
//! ready record asks for that target's pin alone: every other target keeps the build it has,
//! so nothing a pass does can drop a platform.
//!
//! Per program a pass reads `<staging owner>/<repo>`'s main once and FREEZES that commit. It
//! never reads main again: every later step names the frozen commit, and a main that moved
//! meanwhile is the next pass's candidate. A frozen commit whose build the signed index already
//! pins is left alone. Otherwise the pass holds the commit to the qualification list its own
//! repository declares ([`crate::artifact::qualify`]), packs and signs it, runs the packed
//! controls on the unpacked asset, uploads it to the private staging owner and enqueues an
//! immutable ready record ([`super::ready`]). It publishes nothing: the index is the
//! dispatcher's.
//!
//! Nothing a pass starts runs unbounded: each step's command is ended at its committed bound,
//! and the build's bound is the pack's, which ends its build at it. A candidate that fails is
//! counted and latched at [`BLOCK_AFTER`]; one the machine or a host could not serve (the
//! staging owner unreachable, no toolchain lease) is deferred, counted against nothing, and
//! tried again by the next pass.
//!
//! What it leaves alone, by name: a repository the lifecycle registry does not hold active or
//! whose public tier is closed ([`super::lifecycle`]), a coherence-group member (one seal cuts
//! those), a program built from another product's repository (it ships with that product), a
//! program with no row for this machine's target, a program the head does not pin (a first pin
//! is a person's), and a program whose repository declares no qualification list (nothing
//! ships unqualified).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use super::LaneKind;
use super::lifecycle::Registry;
use super::ready::{self, Record, StepRecord};
use super::report::{self, NoticeSink, Transcript};
use super::rows;
use super::state::{LaneState, Lock};
use crate::api::{Build, PackOutcome, PackRequest, Packer, Signing};
use crate::artifact::qualify::{self, Receipt, Runner, Site};
use crate::artifact::tools::{HostTools, Ran, Run, RunError};
use crate::artifact::{pack, upload};
use crate::ctx::Ctx;
use crate::error::{Error, Result};
use crate::host::{HostRole, Slug, gh_serves, preflight_each};
use crate::programs::{self, ProgramDef};
use crate::quad::{self, VerifiedQuad};
use crate::triple::Triple;

/// The transcript's prefix.
pub const ME: &str = "auto-tip";

/// The notification title.
pub const TITLE: &str = "aterm auto-tip";

/// Failed qualifications of one build before it is latched.
pub const BLOCK_AFTER: u32 = 3;

/// The ref a pass freezes a repository's main into, in its own scratch repository.
pub const FROZEN: &str = "refs/atpkg-publish/tip-main";

/// Where a pass unpacks packed assets for their controls, under the lane's state.
pub const UNPACKED: &str = "unpacked";

/// The most bytes a packed asset may unpack to for its controls.
pub const MAX_UNPACK_BYTES: u64 = 16 << 30;

/// The most entries a packed asset may unpack to for its controls.
pub const MAX_UNPACK_ENTRIES: u64 = 1_000_000;

/// How one pass is told to behave.
#[derive(Clone, Debug)]
pub struct Options {
    /// Decide and print the plan per program; write nothing that outlives the run.
    pub dry_run: bool,
    /// Failed qualifications of one build before it is latched.
    pub block_after: u32,
    /// The publication engine's repository, whose main holds the lifecycle registry.
    pub engine: Slug,
    /// The target this machine runs and this pass serves: the pin main is compared with, the
    /// row that is packed and controlled, and the pin its ready records ask for.
    pub host: Triple,
}

impl Options {
    /// The production defaults for `ctx` on a machine that runs `host`.
    #[must_use]
    pub fn new(ctx: &Ctx, dry_run: bool, host: Triple) -> Self {
        Self {
            dry_run,
            block_after: BLOCK_AFTER,
            engine: ctx.staging_slug(super::alab::ENGINE_REPO),
            host,
        }
    }
}

/// One qualified pack, ready for the staging owner.
#[derive(Clone, Debug)]
pub struct Staged<'a> {
    /// The program.
    pub def: &'static ProgramDef,
    /// Its build.
    pub build: Build,
    /// The version its manifest signs.
    pub version: &'a str,
    /// The signed manifest carrying the packed row.
    pub manifest: &'a Path,
    /// Its signature.
    pub signature: &'a Path,
    /// The packed `.tar.zst` of each row the manifest carries.
    pub assets: &'a [PathBuf],
}

/// What a pass does to the world besides packing: read a checkout's recipe, run a
/// qualification command, unpack an asset, and upload a qualified pack.
pub trait Backend: Runner {
    /// `targo metadata --no-deps --format-version 1` of the frozen `checkout`.
    /// # Errors
    /// Why it could not be read.
    fn metadata(&self, ctx: &Ctx, checkout: &Path) -> Result<Vec<u8>>;

    /// Unpack the packed `asset` into the new directory `into`.
    /// # Errors
    /// Why it does not unpack.
    fn unpack(&self, asset: &Path, into: &Path) -> Result<()>;

    /// Upload `staged` to the private staging owner's `atpkg-<program>-<build>` release.
    /// # Errors
    /// An asset that no longer hashes to its signed row, or the upload's refusal.
    fn stage(&self, ctx: &Ctx, staged: &Staged<'_>) -> Result<()>;
}

/// The production backend: this machine's tools and the staging host.
#[derive(Clone, Copy, Debug, Default)]
pub struct HostBackend;

impl Runner for HostBackend {
    fn run(&self, run: &Run) -> std::result::Result<Ran, RunError> {
        Runner::run(&HostTools, run)
    }
}

impl Backend for HostBackend {
    fn metadata(&self, ctx: &Ctx, checkout: &Path) -> Result<Vec<u8>> {
        pack::metadata_json(ctx, &HostTools, checkout)
    }

    fn unpack(&self, asset: &Path, into: &Path) -> Result<()> {
        atpkg::extract_tar_zst(asset, into, MAX_UNPACK_BYTES, MAX_UNPACK_ENTRIES)
            .map_err(|e| Error::refused(format!("unpack {}: {e}", asset.display())))
    }

    fn stage(&self, ctx: &Ctx, staged: &Staged<'_>) -> Result<()> {
        let name = staged.def.name;
        let manifest = rows::local(staged.manifest)?;
        let signed = [
            staged.manifest.to_path_buf(),
            staged.signature.to_path_buf(),
        ];
        for asset in staged.assets {
            let file = asset
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let row = manifest
                .artifacts
                .iter()
                .find(|a| a.asset == file)
                .ok_or_else(|| {
                    Error::refused(format!(
                        "the signed manifest of {name} names no asset {file}"
                    ))
                })?;
            upload::preflight(asset, &row.sha256, &signed)?;
        }
        upload::program_preflight(ctx, name)?;
        let mut files = staged.assets.to_vec();
        files.extend(signed);
        let notes = format!(
            "atpkg tip pack of {name} ({}), qualified by its repository's own list. Signed pkg \
             manifest + tarballs.",
            staged.version
        );
        upload::publish(
            ctx,
            staged.def.repo,
            &upload::tag(name, staged.build),
            staged.manifest,
            &files,
            &notes,
        )
        .map(|_| ())
    }
}

/// What the lane reaches beyond the context.
pub struct Deps<'a> {
    /// Packs plain programs.
    pub packer: &'a dyn Packer,
    /// Reads recipes, runs qualification commands, unpacks and stages.
    pub backend: &'a dyn Backend,
    /// Appends notices to the journal.
    pub notifier: &'a dyn NoticeSink,
}

/// A repository's main, frozen for one pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frozen {
    /// The commit (40 lowercase hex).
    pub sha: String,
    /// Its build number: its commit count.
    pub build: Build,
}

impl Frozen {
    /// The commit's first 12 hex.
    #[must_use]
    pub fn short(&self) -> &str {
        &self.sha[..self.sha.len().min(12)]
    }
}

/// Everything [`decide`] reads about one program.
#[derive(Clone, Debug)]
pub struct Facts {
    /// Why the lane leaves it alone whatever its main says (its group, its lifecycle standing).
    pub refusal: Option<String>,
    /// Its pin on the target the pass serves.
    pub pin: Option<Build>,
    /// Its frozen main, or why none was read. `None`: not looked up.
    pub frozen: Option<std::result::Result<Frozen, String>>,
    /// Whether a ready record already holds this build.
    pub enqueued: bool,
    /// Whether this build is latched BLOCKED.
    pub latched: bool,
}

/// One program's decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Left alone by name: before its main is read, or because the head withdrew (yanked, or
    /// floored out) the build its main froze at.
    Refused(String),
    /// Not pinned by the head.
    Unpinned,
    /// Nothing could be decided this pass.
    Unread(String),
    /// The frozen commit's build is the pin.
    Current,
    /// The pin is ahead of the frozen commit: a pack cut from another line.
    Behind,
    /// A ready record already holds this build; the index is the dispatcher's.
    Enqueued,
    /// Latched after repeated failed qualifications.
    Blocked,
    /// Qualify, pack, stage and enqueue it.
    Qualify,
}

impl Decision {
    /// The table's first word.
    #[must_use]
    pub const fn verdict(&self) -> &'static str {
        match self {
            Self::Refused(_) => "REFUSED",
            Self::Unpinned => "SKIP",
            Self::Unread(_) => "UNREAD",
            Self::Current | Self::Behind => "OK",
            Self::Enqueued => "READY",
            Self::Blocked => "BLOCKED",
            Self::Qualify => "QUALIFY",
        }
    }
}

/// The decision for one program (pure).
#[must_use]
pub fn decide(f: &Facts) -> Decision {
    if let Some(why) = &f.refusal {
        return Decision::Refused(why.clone());
    }
    let Some(pin) = f.pin else {
        return Decision::Unpinned;
    };
    let frozen = match &f.frozen {
        None => return Decision::Unread("its main was not read".into()),
        Some(Err(why)) => return Decision::Unread(why.clone()),
        Some(Ok(frozen)) => frozen,
    };
    if frozen.build == pin {
        return Decision::Current;
    }
    if frozen.build < pin {
        return Decision::Behind;
    }
    if f.enqueued {
        return Decision::Enqueued;
    }
    if f.latched {
        return Decision::Blocked;
    }
    Decision::Qualify
}

/// What became of a candidate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fate {
    /// Qualified, staged and enqueued: its ready record.
    Enqueued(PathBuf),
    /// Its repository declares no qualification list.
    Unqualified,
    /// Why it did not qualify or stage.
    Failed(String),
    /// Why the machine or a host could not serve it this pass: nothing was learned about the
    /// candidate, so nothing is counted against it.
    Deferred(String),
}

/// Why a candidate did not reach the queue.
enum Stop {
    /// The candidate is at fault, or cannot be told from one that is: an attempt.
    Red(String),
    /// The machine or a host could not do its part.
    Infra(String),
}

impl Stop {
    /// A stop for `e`: a could-not-run is the machine's or a host's, anything else the
    /// candidate's.
    fn of(what: &str, e: &Error) -> Self {
        let why = format!("{what}: {e}");
        match e {
            Error::CannotRun(_) => Self::Infra(why),
            _ => Self::Red(why),
        }
    }

    fn words(&self) -> &str {
        match self {
            Self::Red(why) | Self::Infra(why) => why,
        }
    }
}

impl From<String> for Stop {
    fn from(why: String) -> Self {
        Self::Red(why)
    }
}

/// One row of the decision table.
#[derive(Clone, Debug)]
pub struct Row {
    /// The program.
    pub program: &'static ProgramDef,
    /// Its pin.
    pub pin: Option<Build>,
    /// Its frozen main, when one was read.
    pub frozen: Option<Frozen>,
    /// The decision.
    pub decision: Decision,
    /// A dry run's plan for a candidate, one line per action.
    pub plan: Vec<String>,
    /// What became of a candidate in a real run.
    pub fate: Option<Fate>,
}

impl Row {
    /// The decision as the table says it.
    #[must_use]
    pub fn render(&self, head: u64) -> String {
        let pin = self.pin.unwrap_or(0);
        let at = self
            .frozen
            .as_ref()
            .map(|f| format!("main {} (build {})", f.short(), f.build))
            .unwrap_or_default();
        match &self.decision {
            Decision::Refused(why) => format!("REFUSED: {why}"),
            Decision::Unpinned => format!(
                "SKIP: not pinned by index-{head} (a new program is a person's decision: pin it \
                 with `atpkg-publish index --pin {}=<build>`)",
                self.program.name
            ),
            Decision::Unread(why) => format!("UNREAD: {why} — skipped this pass"),
            Decision::Current => format!("OK: pin {pin} is {at}"),
            Decision::Behind => format!(
                "OK: pin {pin} is AHEAD of {at} — a pack cut from another line; nothing to build"
            ),
            Decision::Enqueued => format!(
                "READY: {at} > pin {pin} is qualified and queued — the index is the \
                 dispatcher's"
            ),
            Decision::Blocked => format!(
                "BLOCKED: {at} latched by BLOCKED-{}-{}.md — delete it to retry; a newer main \
                 retries by itself",
                self.program.name,
                self.frozen.as_ref().map_or(0, |f| f.build)
            ),
            Decision::Qualify => format!("QUALIFY: {at} > pin {pin}"),
        }
    }
}

/// What one pass did.
#[derive(Clone, Debug, Default)]
pub struct Outcome {
    /// Another pass held the lock: this one did nothing.
    pub covered: bool,
    /// The verified head it decided over.
    pub head: Option<u64>,
    /// The decision table.
    pub rows: Vec<Row>,
    /// `<program>=<build>` enqueued this pass (or, in a dry run, that a real pass would
    /// qualify).
    pub enqueued: Vec<String>,
    /// `<program>=<build>: why` that failed to qualify or stage.
    pub failed: Vec<String>,
    /// Programs left for another pass or another hand, and why.
    pub skipped: Vec<String>,
}

/// Why a pass serving `host` leaves `def` alone whatever its main says.
#[must_use]
pub fn standing_refusal(def: &ProgramDef, registry: &Registry, host: Triple) -> Option<String> {
    ready::queueable(def.name)
        .err()
        .or_else(|| {
            (!def.darwin_triples().contains(&host)).then(|| {
                format!(
                    "{} ships no {host} row: this machine's target is not one it is built for",
                    def.name
                )
            })
        })
        .or_else(|| registry.standing(def.repo).refusal(def.repo))
}

/// One pass of the tip lane.
///
/// # Errors
/// A refusal for a downgrade, an unverifiable head, a stalled pass, or a pass whose every
/// candidate failed; a could-not-run when the head or the lifecycle registry cannot be read, or
/// the machine lacks what a signing pass needs.
pub fn run(ctx: &Ctx, opts: &Options, deps: &Deps<'_>, tx: &Transcript) -> Result<Outcome> {
    let state = LaneState::of(LaneKind::Tip, &ctx.state_dir);
    if opts.dry_run {
        // No lock, no lane state: every read lands in a private scratch directory that is
        // removed when the run ends. (Reading a recipe holds the toolchain's ordinary lease
        // for that read, as any use of the toolchain does.)
        let scratch = crate::channel::Scratch::new("tip-dry-run")?;
        return pass(ctx, opts, deps, &state, scratch.path(), tx);
    }
    state.ensure()?;
    let Lock::Held(_guard) = state.try_lock()? else {
        if let Some(started) = state.stalled_since(ctx.now_unix) {
            let msg = format!(
                "the pass holding {} started {}, more than {}h ago — a stalled pass (a hung \
                 build? a prompt?); find it with `pgrep -fl 'atpkg-publish( --host [a-z-]+)? \
                 lane run tip'` and end it",
                state.lock.display(),
                aterm_types::rfc3339::format_rfc3339(u64::try_from(started).unwrap_or(0)),
                super::state::STALLED_AFTER_SECS / 3600
            );
            notice(
                ctx,
                deps,
                &state,
                "a stalled pass has held the tip lane lock for more than 12h — see tip.log",
            );
            return Err(Error::refused(msg));
        }
        tx.say(format!(
            "another pass holds {} — it covers this one; exiting 0",
            state.lock.display()
        ));
        return Ok(Outcome {
            covered: true,
            ..Outcome::default()
        });
    };
    state.mark_started(ctx.now_unix);
    state.prune(ctx.now_unix);
    state.prune_evidence(ctx.now_unix);
    prune_unpacked(&state);
    let work = state.dir.join("work");
    let _ = std::fs::remove_dir_all(&work);
    let result = std::fs::create_dir_all(&work)
        .map_err(|e| Error::io(format!("mkdir {}", work.display()), &e))
        .and_then(|()| pass(ctx, opts, deps, &state, &work, tx));
    state.clear_started();
    if let Err(e) = &result {
        notice(ctx, deps, &state, &format!("FAILED: {e}"));
    }
    result
}

/// Remove the unpacked trees of earlier passes whose first-exec hold is over. macOS checks a
/// program's first exec by path after the exec, and removing the file under that check can
/// strand every exec waiting on it, so a tree a control ran from (or was ended in) stays until
/// [`aterm_uds::deadline::copy_may_go`] says the check cannot still be running.
fn prune_unpacked(state: &LaneState) {
    let Ok(entries) = std::fs::read_dir(state.dir.join(UNPACKED)) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let dir = entry.path();
        if entry.file_type().is_ok_and(|kind| kind.is_dir())
            && aterm_uds::deadline::copy_may_go(&dir, now)
        {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

fn notice(ctx: &Ctx, deps: &Deps<'_>, state: &LaneState, message: &str) {
    report::record_once(
        deps.notifier,
        TITLE,
        &state.dir,
        ctx.now_unix,
        false,
        message,
    );
}

fn preflight(ctx: &Ctx, deps: &Deps<'_>, head: &VerifiedQuad, repos: &[&str]) -> Result<()> {
    for (what, path) in [
        ("machine key", &ctx.identity.key),
        ("machine record", &ctx.identity.record),
    ] {
        if !path.is_file() {
            return Err(Error::cannot_run(format!(
                "no {what} at {} — this lane SIGNS what it packs; provision this Mac \
                 (`atpkg-keys join --id <id>`) or run it on a rostered one",
                ctx.shown(path)
            )));
        }
    }
    if gh_serves(ctx, HostRole::Staging) {
        match ctx.usable_token(&ctx.tokens.staging) {
            Ok(true) => {}
            Ok(false) => {
                return Err(Error::cannot_run(format!(
                    "staging token file missing/empty: {} — the unattended lane speaks only as \
                     its owner's token",
                    ctx.shown(&ctx.tokens.staging)
                )));
            }
            Err(why) => {
                return Err(Error::cannot_run(format!(
                    "staging token file invalid/unreadable: {} ({why})",
                    ctx.shown(&ctx.tokens.staging)
                )));
            }
        }
    } else {
        preflight_each(
            &*ctx.hosts.staging,
            repos.iter().map(|repo| ctx.staging_slug(repo)),
        )
        .map_err(|(slug, e)| {
            Error::cannot_run(format!(
                "staging writer unavailable on {slug}: {e} — the unattended lane writes only \
                 through agh.toml's writers"
            ))
        })?;
    }
    deps.packer
        .can_pack()
        .map_err(|why| Error::cannot_run(format!("this Mac cannot pack: {why}")))?;
    crate::identity::signer_for(ctx, &head.roster)?;
    crate::artifact::sign::elect_from_head(ctx, head);
    Ok(())
}

/// Where one pass reads and builds.
struct Bench<'a> {
    ctx: &'a Ctx,
    opts: &'a Options,
    deps: &'a Deps<'a>,
    state: &'a LaneState,
    work: &'a Path,
    tx: &'a Transcript,
    /// Each repository's main, frozen at most once per pass.
    frozen: BTreeMap<String, std::result::Result<(PathBuf, Frozen), String>>,
}

impl Bench<'_> {
    /// The repository main is frozen in. A real pass keeps one scratch repository per program
    /// repository under the lane's state, so a later pass fetches only what moved. A dry run
    /// makes a bare one in its scratch directory, borrowing objects read-only from the lane's
    /// scratch repository and from the operator's own checkout (`~/<repo>`) where they exist.
    fn repository(&self, def: &ProgramDef) -> std::result::Result<PathBuf, String> {
        let kept = self.state.dir.join("progs").join(def.repo);
        let (dir, init): (PathBuf, &[&str]) = if self.opts.dry_run {
            (
                self.work.join("progs").join(format!("{}.git", def.repo)),
                &["init", "--quiet", "--bare"],
            )
        } else {
            (kept.clone(), &["init", "--quiet"])
        };
        let made = if dir.join(".git").exists() || dir.join("HEAD").exists() {
            false
        } else {
            std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
            crate::git::run(&dir, init).map_err(|e| e.to_string())?;
            true
        };
        if made && self.opts.dry_run {
            // Borrowed objects only save a transfer: the commit frozen is still the one the
            // host names as main.
            let borrowed: String = [kept, self.ctx.home.join(def.repo)]
                .iter()
                .map(|repo| repo.join(".git/objects"))
                .filter(|objects| objects.is_dir())
                .map(|objects| format!("{}\n", objects.display()))
                .collect();
            if !borrowed.is_empty() {
                std::fs::write(dir.join("objects/info/alternates"), borrowed)
                    .map_err(|e| format!("borrow objects for {}: {e}", def.repo))?;
            }
        }
        if made {
            let config = if self.opts.dry_run {
                dir.clone()
            } else {
                dir.join(".git")
            };
            super::alab::owner_written(&config)
                .map_err(|e| format!("clear the write bits of {}: {e}", config.display()))?;
        }
        if made && !self.opts.dry_run {
            // Says whose source this is (a harness pack holds its checkout's origin to its
            // repository). Nothing is ever fetched from it: main is read through the host.
            let origin = format!("https://github.com/{}.git", self.ctx.staging_slug(def.repo));
            crate::git::run(&dir, &["remote", "add", "origin", &origin])
                .map_err(|e| e.to_string())?;
        }
        Ok(dir)
    }

    /// Read `def`'s main of record through the staging host and freeze it: the one read of
    /// main this pass makes for its repository.
    fn freeze(&mut self, def: &ProgramDef) -> std::result::Result<(PathBuf, Frozen), String> {
        if let Some(known) = self.frozen.get(def.repo) {
            return known.clone();
        }
        let read = self.repository(def).and_then(|dir| {
            let slug = self.ctx.staging_slug(def.repo);
            self.ctx
                .hosts
                .staging
                .fetch_main(&slug, &dir, FROZEN)
                .map_err(|e| format!("could not read {slug}'s main from the host ({e})"))?;
            let sha = crate::git::run(
                &dir,
                &["rev-parse", "--verify", &format!("{FROZEN}^{{commit}}")],
            )
            .map_err(|e| format!("{slug}'s main was fetched but cannot be read: {e}"))?;
            if sha.len() != 40 || !sha.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
                return Err(format!("{slug}'s main read as `{sha}`, not a full commit"));
            }
            let build = crate::git::rev_count(&dir, &sha).map_err(|e| e.to_string())?;
            Ok((dir, Frozen { sha, build }))
        });
        self.frozen.insert(def.repo.to_string(), read.clone());
        read
    }

    fn gather(
        &mut self,
        def: &'static ProgramDef,
        registry: &Registry,
        view: &atpkg::manifest::Channel,
        pins: &BTreeMap<String, Build>,
    ) -> Row {
        let mut facts = Facts {
            refusal: standing_refusal(def, registry, self.opts.host),
            pin: pins.get(def.name).copied(),
            frozen: None,
            enqueued: false,
            latched: false,
        };
        let mut row = Row {
            program: def,
            pin: facts.pin,
            frozen: None,
            decision: Decision::Unpinned,
            plan: Vec::new(),
            fate: None,
        };
        // A refused or unpinned program's repository is never even read.
        if let (None, Some(pin)) = (&facts.refusal, facts.pin) {
            let frozen = self.freeze(def).map(|(_, frozen)| frozen);
            if let Ok(frozen) = &frozen {
                row.frozen = Some(frozen.clone());
                if frozen.build > pin {
                    // A build the head withdrew is never qualified: no index may pin it.
                    facts.refusal = ready::withdrawn(view, def.name, frozen.build);
                    facts.enqueued = self
                        .state
                        .ready_dir()
                        .join(ready_name(def.name, frozen.build))
                        .is_file();
                    facts.latched = self.state.blocked_file(def.name, frozen.build).exists();
                }
            }
            facts.frozen = Some(frozen);
        }
        row.decision = decide(&facts);
        row
    }
}

fn ready_name(program: &str, build: Build) -> String {
    format!("{program}-{build}{}", ready::SUFFIX)
}

#[allow(clippy::too_many_lines)]
fn pass(
    ctx: &Ctx,
    opts: &Options,
    deps: &Deps<'_>,
    state: &LaneState,
    work: &Path,
    tx: &Transcript,
) -> Result<Outcome> {
    tx.raw(format!(
        "=== {ME} {} pid {}{} ===",
        aterm_types::rfc3339::format_rfc3339(u64::try_from(ctx.now_unix).unwrap_or(0)),
        std::process::id(),
        if opts.dry_run { " (DRY_RUN)" } else { "" }
    ));
    let slug = ctx.index_slug();
    let (n, head) = match quad::fetch_head(ctx, &slug, &work.join("head")) {
        Ok(h) => h,
        Err(Error::CannotRun(why)) => {
            let fails = state.bump_head_failures(opts.dry_run);
            return Err(Error::cannot_run(format!(
                "could not read an atpkg-index-N release on {slug} ({why}; consecutive \
                 failures: {fails}) — no channel head to decide against"
            )));
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
    let seen = state.check_index(n)?;
    let channel = head.index.channels.first().cloned().ok_or_else(|| {
        Error::refused(format!(
            "index-{n} carries no [[channels]] block — no pins to track"
        ))
    })?;
    // One target's view of the channel: the head may pin another target at another build
    // (a `pin_by_target` overlay), and that is none of this pass's business.
    let served = [opts.host];
    let view = channel.for_target(opts.host.as_str());
    let pins = crate::pins::for_targets(&channel, &served)?;
    state.check_pins(&served, &pins, n, tx)?;
    let (at, registry) = super::lifecycle::read(ctx, &opts.engine, work).map_err(|why| {
        Error::cannot_run(format!(
            "the lifecycle registry could not be read ({why}) — nothing is built without it"
        ))
    })?;
    tx.say(format!(
        "channel index head: atpkg-index-{n} (last processed: {seen}), as {} sees it; \
         lifecycle registry: {at}",
        opts.host
    ));

    let mut bench = Bench {
        ctx,
        opts,
        deps,
        state,
        work,
        tx,
        frozen: BTreeMap::new(),
    };
    let mut rows: Vec<Row> = programs::PROGRAMS
        .iter()
        .filter(|def| def.is_plain())
        .map(|def| bench.gather(def, &registry, &view, &pins))
        .collect();
    say_table(tx, n, &rows);

    let mut outcome = Outcome {
        head: Some(n),
        ..Outcome::default()
    };
    for row in &rows {
        match &row.decision {
            Decision::Refused(why) | Decision::Unread(why) => {
                outcome.skipped.push(format!("{}: {why}", row.program.name));
            }
            Decision::Blocked => outcome
                .skipped
                .push(format!("{}: latched BLOCKED", row.program.name)),
            _ => {}
        }
    }
    let candidates: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| row.decision == Decision::Qualify)
        .map(|(i, _)| i)
        .collect();

    if opts.dry_run {
        let mut unlisted = Vec::new();
        for &i in &candidates {
            let (lines, listed) = bench.plan(&rows[i]);
            for line in &lines {
                tx.raw(format!("    {line}"));
            }
            let label = format!(
                "{}={}",
                rows[i].program.name,
                rows[i].frozen.as_ref().map_or(0, |f| f.build)
            );
            if listed {
                outcome.enqueued.push(label);
            } else {
                outcome
                    .skipped
                    .push(format!("{label}: no qualification list to run"));
                unlisted.push(label);
            }
            rows[i].plan = lines;
        }
        tx.say(format!(
            "DRY_RUN — would qualify, pack, stage and enqueue: {}; ahead of their pin with no \
             qualification list to run: {}; no clone, build, sign, upload, ready record or \
             lane state was written",
            list(outcome.enqueued.iter().map(String::as_str)),
            list(unlisted.iter().map(String::as_str))
        ));
        outcome.rows = rows;
        return Ok(outcome);
    }

    if candidates.is_empty() {
        raise_floors(state, n, &served, &pins)?;
        tx.say("every tracked program's main is pinned, queued or left alone — clean no-op");
        outcome.rows = rows;
        return Ok(outcome);
    }
    let repos: Vec<&str> = candidates.iter().map(|&i| rows[i].program.repo).collect();
    preflight(ctx, deps, &head, &repos)?;
    for &i in &candidates {
        let fate = bench.qualify(&rows[i], n);
        let name = rows[i].program.name;
        let build = rows[i].frozen.as_ref().map_or(0, |f| f.build);
        match &fate {
            Fate::Enqueued(record) => {
                state.clear_attempts(name, build);
                tx.say(format!(
                    "[{name}] build {build} is qualified, staged and queued: {}",
                    ctx.shown(record)
                ));
                outcome.enqueued.push(format!("{name}={build}"));
            }
            Fate::Unqualified => {
                let why = format!(
                    "{name}: its repository declares no [workspace.metadata.atpkg] qualify \
                     list at {} — nothing ships unqualified",
                    rows[i].frozen.as_ref().map_or("?", Frozen::short)
                );
                tx.warn(&why);
                outcome.skipped.push(why);
            }
            Fate::Failed(headline) => {
                bench.fail(name, build, headline);
                outcome.failed.push(format!("{name}={build}: {headline}"));
            }
            Fate::Deferred(why) => {
                let line = format!(
                    "{name}={build}: {why} — nothing is counted against the build; the next \
                     pass tries it again"
                );
                tx.warn(&line);
                notice(ctx, deps, state, &line);
                outcome.skipped.push(line);
            }
        }
        rows[i].fate = Some(fate);
    }
    raise_floors(state, n, &served, &pins)?;
    outcome.rows = rows;
    if outcome.enqueued.is_empty() && !outcome.failed.is_empty() {
        return Err(Error::refused(format!(
            "every candidate failed to qualify — see the receipts, logs and attempts/BLOCKED \
             markers under {}: {}",
            ctx.shown(&state.dir),
            outcome.failed.join("; ")
        )));
    }
    if !outcome.enqueued.is_empty() {
        notice(
            ctx,
            deps,
            state,
            &format!("queued for the index: {}", outcome.enqueued.join(" ")),
        );
    }
    tx.say(format!(
        "done — queued: {}; failed: {}; the index is the dispatcher's (`lane run tip \
         --staged-only`)",
        list(outcome.enqueued.iter().map(String::as_str)),
        list(outcome.failed.iter().map(String::as_str))
    ));
    Ok(outcome)
}

/// Raise the lane's floors to what this pass read. The dispatcher records the same floors
/// under its own lock while a pass runs, from a head at least as new: the pass writes under
/// that lock and never below what is there, and leaves them to a dispatch in flight.
fn raise_floors(
    state: &LaneState,
    index: u64,
    served: &[Triple],
    pins: &BTreeMap<String, Build>,
) -> Result<()> {
    match state.queued_lock()? {
        Lock::Held(_guard) => state.raise(index, served, pins),
        Lock::Busy => Ok(()),
    }
}

fn say_table(tx: &Transcript, n: u64, rows: &[Row]) {
    tx.say(format!(
        "decision table for atpkg-index-{n} (each program's dev main, frozen once):"
    ));
    let mut table = vec![vec![
        "PROGRAM".to_string(),
        "PINNED".into(),
        "MAIN".into(),
        "MAIN-BUILD".into(),
        "DECISION".into(),
    ]];
    for r in rows {
        table.push(vec![
            r.program.name.to_string(),
            r.pin.map_or_else(|| "—".into(), |p| p.to_string()),
            r.frozen
                .as_ref()
                .map_or_else(|| "—".into(), |f| f.short().to_string()),
            r.frozen
                .as_ref()
                .map_or_else(|| "—".into(), |f| f.build.to_string()),
            r.render(n),
        ]);
    }
    for line in report::columns(&table) {
        tx.raw(line);
    }
}

fn list<'a>(items: impl IntoIterator<Item = &'a str>) -> String {
    let v: Vec<&str> = items.into_iter().collect();
    if v.is_empty() {
        "<none>".into()
    } else {
        v.join("; ")
    }
}

/// What a qualification left, for a person or the next agent to read.
#[derive(serde::Serialize)]
struct Report<'a> {
    schema: &'static str,
    program: &'a str,
    repo: &'a str,
    source: &'a str,
    build: Build,
    outcome: &'a str,
    why: Option<&'a str>,
    steps: Vec<StepLine>,
}

#[derive(serde::Serialize)]
struct StepLine {
    name: String,
    stage: &'static str,
    target: Option<String>,
    argv: Vec<String>,
    exit: Option<i32>,
    millis: u64,
    within_secs: u64,
    stdout_sha256: String,
    stderr_sha256: String,
    tests_ran: Option<u64>,
    tests_passed: Option<u64>,
    passed: bool,
    why: Option<String>,
}

impl StepLine {
    fn of(r: &Receipt) -> Self {
        Self {
            name: r.name.clone(),
            stage: r.stage.as_str(),
            target: r.target.map(|t| t.as_str().to_string()),
            argv: r.argv.clone(),
            exit: r.exit,
            millis: r.millis,
            within_secs: r.within_secs,
            stdout_sha256: r.stdout_sha256.clone(),
            stderr_sha256: r.stderr_sha256.clone(),
            tests_ran: r.tests.map(qualify::Tests::ran),
            tests_passed: r.tests.map(|t| t.passed),
            passed: r.passed(),
            why: match &r.verdict {
                qualify::Verdict::Passed => None,
                qualify::Verdict::Failed(why) => Some(why.clone()),
            },
        }
    }
}

impl Bench<'_> {
    /// A dry run's plan for one candidate: what a real pass would do, in order, and whether
    /// its repository declares a list to run. The recipe is read from the frozen commit,
    /// checked out in the run's scratch directory.
    fn plan(&mut self, row: &Row) -> (Vec<String>, bool) {
        let def = row.program;
        let name = def.name;
        let (Some(frozen), Ok((dir, _))) = (&row.frozen, self.freeze(def)) else {
            return (vec![format!("[{name}] no frozen main to plan from")], false);
        };
        let mut lines = vec![format!(
            "[{name}] PLAN — freeze {} main at {} (build {} > pin {})",
            self.ctx.staging_slug(def.repo),
            frozen.sha,
            frozen.build,
            row.pin.unwrap_or(0)
        )];
        let src = self.work.join("src").join(def.repo);
        let listed = (|| {
            if !may_declare_list(&dir, frozen).map_err(Error::refused)? {
                return Ok(None);
            }
            if !src.is_dir() {
                crate::git::run(
                    &dir,
                    &[
                        "worktree",
                        "add",
                        "--quiet",
                        "--detach",
                        &src.display().to_string(),
                        &frozen.sha,
                    ],
                )?;
            }
            qualify::parse(&self.deps.backend.metadata(self.ctx, &src)?, name)
        })();
        let qualified = matches!(listed, Ok(Some(_)));
        match listed {
            Ok(Some(plan)) => {
                lines.extend(plan.lines().into_iter().map(|l| format!("[{name}]   {l}")));
                lines.push(format!(
                    "[{name}]   pack {} signed with this machine's key, and run its packed \
                     controls here",
                    self.opts.host
                ));
                lines.push(format!(
                    "[{name}]   upload {} to {} (private prerelease), then enqueue {} for the \
                     {} pin",
                    upload::tag(name, frozen.build),
                    self.ctx.staging_slug(def.repo),
                    ready_name(name, frozen.build),
                    self.opts.host
                ));
            }
            Ok(None) => lines.push(format!(
                "[{name}]   NOT QUALIFIED — its repository declares no \
                 [workspace.metadata.atpkg] qualify list at this commit; a real pass stops here"
            )),
            Err(e) => lines.push(format!(
                "[{name}]   the qualification list could not be read: {e}"
            )),
        }
        (lines, qualified)
    }

    /// Hold one frozen candidate to its repository's list, pack it, run its packed controls,
    /// stage it and enqueue its ready record.
    fn qualify(&mut self, row: &Row, head: u64) -> Fate {
        let def = row.program;
        let name = def.name;
        let (Some(frozen), Decision::Qualify) = (&row.frozen, &row.decision) else {
            return Fate::Failed("not a candidate".into());
        };
        let clone = match self.freeze(def) {
            Ok((dir, again)) if again == *frozen => dir,
            Ok(_) => return Fate::Failed("its frozen main changed within the pass".into()),
            Err(why) => return Fate::Failed(why),
        };
        let mut receipts = Vec::new();
        let done = self.qualify_frozen(def, frozen, &clone, head, row, &mut receipts);
        let (outcome, why, fate) = match done {
            Ok(Some(record)) => ("enqueued", None, Fate::Enqueued(record)),
            Ok(None) => ("unqualified", None, Fate::Unqualified),
            Err(Stop::Red(why)) => ("failed", Some(why.clone()), Fate::Failed(why)),
            Err(Stop::Infra(why)) => ("deferred", Some(why.clone()), Fate::Deferred(why)),
        };
        for receipt in &receipts {
            self.tx.say(format!("[{name}] {}", receipt.line()));
        }
        let report = Report {
            schema: "atpkg.lane.qualification.v1",
            program: name,
            repo: def.repo,
            source: &frozen.sha,
            build: frozen.build,
            outcome,
            why: why.as_deref(),
            steps: receipts.iter().map(StepLine::of).collect(),
        };
        let path = self
            .state
            .dir
            .join("receipts")
            .join(format!("{name}-{}.json", frozen.build));
        let written = aterm_json::to_string(&report)
            .map_err(|e| Error::refused(format!("encode the qualification receipt: {e}")))
            .and_then(|text| report::write_atomic(&path, &text));
        if let Err(e) = written {
            self.tx.warn(format!(
                "[{name}] its qualification receipt was not kept: {e}"
            ));
        }
        fate
    }

    /// `Ok(Some(record))` enqueued, `Ok(None)` no list declared, `Err(stop)` failed or deferred.
    #[allow(clippy::too_many_lines)]
    fn qualify_frozen(
        &self,
        def: &'static ProgramDef,
        frozen: &Frozen,
        clone: &Path,
        head: u64,
        row: &Row,
        receipts: &mut Vec<Receipt>,
    ) -> std::result::Result<Option<PathBuf>, Stop> {
        let (ctx, tx, name, build) = (self.ctx, self.tx, def.name, frozen.build);
        if !may_declare_list(clone, frozen)? {
            return Ok(None);
        }
        tx.say(format!(
            "[{name}] main frozen at {} (build {build}); checking it out in {}",
            frozen.sha,
            ctx.shown(clone)
        ));
        crate::git::run(clone, &["checkout", "--force", "--detach", &frozen.sha])
            .map_err(|e| format!("checkout of {} failed: {e}", frozen.short()))?;
        scrub(clone, frozen)?;

        let json = self
            .deps
            .backend
            .metadata(ctx, clone)
            .map_err(|e| Stop::of("its recipe could not be read", &e))?;
        let Some(plan) = qualify::parse(&json, name).map_err(|e| e.to_string())? else {
            return Ok(None);
        };
        // Each attempt keeps its own raw output: a retry never overwrites what the last one
        // left to be read.
        let logs = self
            .state
            .dir
            .join("logs")
            .join(format!("{name}-{build}"))
            .join(format!(
                "attempt-{}",
                self.state.attempts(name, build).saturating_add(1)
            ));
        let _ = std::fs::remove_dir_all(&logs);

        // Cheap first: the source checks, in the order the repository wrote them.
        let source = Site {
            cwd: clone,
            packed: None,
            logs: &logs,
        };
        let ran = qualify::run_steps(self.deps.backend, plan.source(), &source)
            .map_err(|e| Stop::of("a step's output could not be kept", &e))?;
        let red = ran.iter().find(|r| !r.passed()).map(Receipt::line);
        receipts.extend(ran);
        if let Some(line) = red {
            return Err(line.into());
        }
        // A step may leave files no commit holds (a lock file, generated source, a
        // `.cargo/config.toml`), and a build reads those exactly as it reads tracked ones:
        // what is built is the frozen commit's tree alone.
        scrub(clone, frozen)?;

        // The build: the pack itself, of this machine's target.
        let out = self.state.dir.join("out").join(name);
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).map_err(|e| format!("mkdir {}: {e}", out.display()))?;
        let started = Instant::now();
        let packed = self.pack(def, clone, build, &out, plan.build().within);
        let receipt = Receipt::build(
            plan.build(),
            started.elapsed(),
            packed
                .as_ref()
                .map(|_| ())
                .map_err(|stop| stop.words().to_string()),
        );
        let red = (!receipt.passed()).then(|| receipt.line());
        receipts.push(receipt);
        let leg = match (packed, red) {
            (Err(infra @ Stop::Infra(_)), _) => return Err(infra),
            (_, Some(line)) => return Err(line.into()),
            (Ok(leg), None) => leg,
            (Err(stop), None) => return Err(stop),
        };
        intact(clone, frozen)?;
        let signature = leg
            .signature
            .clone()
            .ok_or_else(|| "the pack left its manifest unsigned".to_string())?;
        let carried = rows::local(&leg.manifest)
            .map(|m| rows::targets(&m))
            .map_err(|e| e.to_string())?;
        if !carried.contains(&self.opts.host) {
            return Err(format!("the packed manifest carries no {} row", self.opts.host).into());
        }

        // The controls, on the packed asset, from outside the checkout. The unpacked tree
        // holds programs this machine has never run: it gets a directory of its own that is
        // stamped before their first exec and removed only once macOS can no longer be
        // checking one ([`prune_unpacked`]).
        let unpacked = self.state.dir.join(UNPACKED);
        std::fs::create_dir_all(&unpacked)
            .map_err(|e| Stop::Infra(format!("mkdir {}: {e}", unpacked.display())))?;
        let root = aterm_tempfile::Builder::new()
            .prefix(&format!("{name}-{build}-{}.", leg.triple))
            .tempdir_in(&unpacked)
            .map_err(|e| Stop::Infra(format!("reserve a directory to unpack into: {e}")))?
            .keep();
        self.deps
            .backend
            .unpack(&leg.asset, &root)
            .map_err(|e| format!("the {} asset does not unpack: {e}", leg.triple))?;
        aterm_uds::deadline::stamp_exec(&root);
        let cwd = self.work.join("controls").join(name);
        std::fs::create_dir_all(&cwd)
            .map_err(|e| Stop::Infra(format!("mkdir {}: {e}", cwd.display())))?;
        let site = Site {
            cwd: &cwd,
            packed: Some((&root, leg.triple)),
            logs: &logs,
        };
        let ran = qualify::run_steps(self.deps.backend, plan.packed(), &site)
            .map_err(|e| Stop::of("a step's output could not be kept", &e))?;
        let red = ran.iter().find(|r| !r.passed()).map(Receipt::line);
        receipts.extend(ran);
        if let Some(line) = red {
            return Err(line.into());
        }

        // Qualified: only now does anything leave this machine.
        let assets = [leg.asset.clone()];
        let manifest_sha256 = atpkg::tree::sha256_file(&leg.manifest)
            .map_err(|e| format!("hash {}: {e}", leg.manifest.display()))?;
        self.deps
            .backend
            .stage(
                ctx,
                &Staged {
                    def,
                    build,
                    version: &leg.version,
                    manifest: &leg.manifest,
                    signature: &signature,
                    assets: &assets,
                },
            )
            .map_err(|e| Stop::of("staging refused it", &e))?;
        tx.say(format!(
            "[{name}] staged {} on {} ({})",
            upload::tag(name, build),
            ctx.staging_slug(def.repo),
            super::state::names(&carried.iter().copied().collect::<Vec<_>>())
        ));
        let record = Record {
            schema: ready::SCHEMA.to_string(),
            program: name.to_string(),
            repo: def.repo.to_string(),
            source: frozen.sha.clone(),
            build,
            version: leg.version.clone(),
            target: self.opts.host.as_str().to_string(),
            targets: carried.iter().map(|t| t.as_str().to_string()).collect(),
            manifest_sha256,
            pin_before: row.pin.unwrap_or(0),
            decided_over_index: head,
            qualified_unix: ctx.now_unix,
            steps: receipts.iter().filter_map(StepRecord::of).collect(),
        };
        ready::enqueue(self.state, &record)
            .map(Some)
            .map_err(|e| Stop::of("its ready record was not enqueued", &e))
    }

    /// Pack this machine's target of `def` at `build` from `clone`, its build ended at
    /// `within`. Signed, and not uploaded: nothing leaves this machine before its packed
    /// controls pass.
    fn pack(
        &self,
        def: &ProgramDef,
        clone: &Path,
        build: Build,
        out: &Path,
        within: std::time::Duration,
    ) -> std::result::Result<PackOutcome, Stop> {
        let (name, triple) = (def.name, self.opts.host);
        self.tx
            .say(format!("[{name}] packing {triple} (build {build})"));
        let outcome = self
            .deps
            .packer
            .pack(
                self.ctx,
                &PackRequest {
                    program: name.to_string(),
                    checkout: clone.to_path_buf(),
                    triple,
                    build: Some(build),
                    merge_from: None,
                    toolchain: None,
                    out: out.join(triple.as_str()),
                    signing: Signing::MachineKey,
                    upload: false,
                    env: Vec::new(),
                    build_within: Some(within),
                },
            )
            .map_err(|e| Stop::of(&format!("the pack failed for {triple}"), &e))?;
        if outcome.build != build {
            return Err(format!(
                "the {triple} pack reports build {}, not {build}",
                outcome.build
            )
            .into());
        }
        Ok(outcome)
    }

    /// Count a failed qualification of `program` `build`, and latch it at the bound.
    fn fail(&self, program: &str, build: Build, headline: &str) {
        let (ctx, tx, state) = (self.ctx, self.tx, self.state);
        let n = state.bump_attempts(program, build);
        if n < self.opts.block_after {
            tx.warn(format!(
                "{program} build {build}: {headline} (attempt {n} of {} — retrying next pass)",
                self.opts.block_after
            ));
            notice(
                ctx,
                self.deps,
                state,
                &format!(
                    "{program} build {build}: not qualified (attempt {n} of {}) — {headline}",
                    self.opts.block_after
                ),
            );
            return;
        }
        let marker = state.blocked_file(program, build);
        let text = format!(
            "# auto-tip: {program} build {build} BLOCKED after {n} attempts\n\n{headline}\n\n- \
             this file is the latch: the lane skips {program} build {build} without rebuilding \
             while it exists.\n  Delete it (and attempts-{program}-{build}) to retry; a newer \
             main retries by itself.\n- the raw output of every step is under \
             logs/{program}-{build}/attempt-<n>/, and its receipt is \
             receipts/{program}-{build}.json.\n"
        );
        let _ = report::write_atomic(&marker, &text);
        tx.warn(format!(
            "{program} build {build} BLOCKED after {n} attempts — {headline} (marker: {})",
            ctx.shown(&marker)
        ));
        notice(
            ctx,
            self.deps,
            state,
            &format!("{program} build {build} blocked after {n} attempts: {headline}"),
        );
    }
}

/// Whether the frozen commit could declare a qualification list at all: some `Cargo.toml` of
/// it spells `qualify`. Read from the commit's objects, so a repository that has not joined
/// the lane costs a pass one `git grep`, never a checkout and a `targo metadata`. A list is
/// still read only from the recipe ([`qualify::parse`]): this never says a list is there.
fn may_declare_list(repository: &Path, frozen: &Frozen) -> std::result::Result<bool, String> {
    crate::git::tree_holds(
        repository,
        &frozen.sha,
        "qualify",
        &["Cargo.toml", "*/Cargo.toml"],
    )
    .map_err(|e| format!("its manifests could not be read at {}: {e}", frozen.short()))
}

/// Make the scratch tree the frozen commit's alone: no tracked file changed ([`intact`]), and
/// nothing else in it but the top-level `target/` (untracked and ignored files are removed,
/// and what a fresh clone would not hold is refused).
fn scrub(clone: &Path, frozen: &Frozen) -> std::result::Result<(), String> {
    intact(clone, frozen)?;
    crate::git::run(clone, &["clean", "-ffdxq", "-e", "/target"])
        .map_err(|e| format!("could not clear the scratch tree: {e}"))?;
    match crate::git::is_pristine(clone).map_err(|e| e.to_string())? {
        None => Ok(()),
        Some(stray) => Err(format!(
            "the scratch tree is not the frozen commit's alone ({stray})"
        )),
    }
}

/// The scratch tree is still the frozen commit, with no tracked file changed: what was checked
/// is what gets built, and what was built is what gets staged.
fn intact(clone: &Path, frozen: &Frozen) -> std::result::Result<(), String> {
    let head = crate::git::head(clone).map_err(|e| e.to_string())?;
    if head != frozen.sha {
        return Err(format!(
            "the scratch tree moved off the frozen commit {} (now {head}) — refusing its \
             artifact",
            frozen.short()
        ));
    }
    let changed = crate::git::run(clone, &["status", "--porcelain", "--untracked-files=no"])
        .map_err(|e| e.to_string())?;
    if let Some(first) = changed.lines().next() {
        return Err(format!(
            "a step changed tracked source ({}) — what was checked is not what would be built",
            first.trim()
        ));
    }
    Ok(())
}
