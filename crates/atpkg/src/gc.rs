// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Rollback-aware GC retention (§10.2): which store builds may be reclaimed, and the single
//! destructive entry point that decision goes through.
//!
//! Retention is **live + 1 rollback** per program. A superseded build is never reclaimed
//! while it is the live build or the rollback target (reclaiming the latter resurrects the
//! cryptic *"Reading release bundle rust-toolchain-version: No such file or directory"*
//! failure `scripts/verify-kani-proofs.sh` guards against), so [`reclaimable`] keeps the live build and the
//! single most-recent build below it; everything else is safe to delete.
//!
//! **The live build is a WITNESS, not an inference — that distinction is the whole module.**
//! GC used to ask [`crate::ops::active_builds`] which build was current. That function folds
//! the `bin/` shims into one entry per program with `insert` in a `read_dir` loop: when a
//! program's shims disagree, *which* build it reports depends on directory iteration order.
//! Feed an OLDER build in as `current` and the retention rule dutifully classifies everything
//! above it — including the build the channel actually selects — as superseded, and the
//! unconditional `remove_dir_all` deletes the user's live toolchain. That is a bricking path,
//! and it was reachable through a plain `atpkg install`.
//!
//! So the type system now forbids the shape:
//!
//! * [`LiveBuild`] is evidence, with private fields and exactly one producer
//!   ([`live_builds`]), which resolves the `current` symlinks activation itself writes — and
//!   requires the derived `bin/` view to agree with them.
//! * [`reclaimable`] takes that witness instead of a bare `u64`, so a shim-inferred number
//!   cannot be passed.
//! * [`discard_superseded`] is the only reclaim of an INSTALLED build (see the second
//!   destructive path below for the trees that were never installed at all);
//!   `store::discard_build` is `pub(crate)` (so
//!   the raw delete cannot be *written* outside this crate without first producing a witness).
//!   It is a *last-ditch* guard, not the retention rule: it refuses only the witness's own
//!   build. **The rule — live + one rollback — lives in [`reclaimable`], and only there.**
//!   Hand `discard_superseded` the rollback target and it deletes it.
//!
//! **The authority is `store/<program>/current`, not `channels/<c>/current`.** The channel
//! link is one symlink per CHANNEL and every program shares a channel name (default
//! `stable`), so it is overwritten by whichever program was activated last: as the sole
//! authority it witnesses exactly one program and GC abstains on all the others *forever*,
//! which is a silent unbounded-growth bug rather than a safe one. So
//! [`crate::activate::activate_channel`] writes a per-program link too, and that is the
//! authority — where it answers, the channel links are not consulted at all. They are read
//! only for a program it does NOT answer for, which is exactly a prefix last written by a
//! manager older than the per-program link: a self-limiting migration that keeps the
//! last-activated program witnessed across the upgrade and expires at its next activation.
//!
//! There is a SECOND destructive path, and it is deliberately NOT witness-guarded, because it
//! cannot be. An install killed mid-extract leaves a marker-less build tree that
//! [`crate::ops::list_installed`] cannot see, and an interrupted *fresh* install has no live
//! build to witness at all — so a witness-gated sweep would abstain forever on exactly the
//! trees that leak (a toolchain is gigabytes). `interrupted_debris` is therefore guarded on
//! CLAIMS: the union of every authoritative `current` link and every `bin/` shim target. A
//! build any link or shim resolves into is never swept, however partial it looks. That is
//! strictly stronger evidence of not-live than the missing marker, which on its own proves
//! nothing — `doctor` reports "active build store missing/incomplete" precisely because a LIVE
//! build can lose its marker (a restore, an interrupted fs op, a hand-edited prefix), and
//! sweeping on the marker alone would `remove_dir_all` the running toolchain.
//!
//! And it **fails closed**: unreadable link directories, a dangling or out-of-store
//! `current`, authorities that disagree, or shims that disagree all yield NO witness, and a
//! program without a witness is skipped entirely. There is deliberately no "no witness ⇒ fall
//! back to the shim map" path — that is the original bug with an extra step. What fail-closed
//! costs is that a disagreement becomes permanent unless someone is told, so every skip is
//! reported as a [`Divergence`] and `atpkg doctor` prints it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::store::Layout;

/// EVIDENCE that `build` is the live build of `program`.
///
/// The fields are private and [`live_builds`] is the only constructor, so this value cannot
/// be minted from [`crate::ops::active_builds`]' `read_dir`-ordered map — which is exactly
/// what the old `reclaimable(installed, current: u64, ..)` signature accepted. Holding one
/// means the prefix *proved* the claim: every authoritative `current` symlink that resolves
/// into `store/<program>/` names the same `<build>`, and no `bin/` shim of that program
/// contradicts it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LiveBuild {
    program: String,
    build: u64,
}

impl LiveBuild {
    /// The program this witness is about.
    #[must_use]
    pub fn program(&self) -> &str {
        &self.program
    }

    /// The build proven live.
    #[must_use]
    pub fn build(&self) -> u64 {
        self.build
    }
}

/// Why a program has no live witness. Each variant is a state in which deleting anything
/// would be a guess, so GC abstains and `doctor` explains which of the two views is wrong.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Diverged {
    /// The activation authority selects one build; the program's `bin/` shims run another.
    /// Whichever is stale, the user is executing a build activation does not select — and the
    /// *old* retention rule would have deleted whichever of the two the loser superseded.
    ChannelShimMismatch { channel_says: u64, shims_say: u64 },
    /// This program's own tools resolve into different builds. The state
    /// [`crate::activate::install_shims`]' prune exists to prevent, still reachable when its
    /// per-tool loop fails partway through (activate.rs `?`s before the prune runs).
    ShimsDisagree { builds: Vec<u64> },
    /// Two `channels/<c>/current` links select different builds of the same program, and the
    /// program has no `store/<program>/current` of its own to break the tie (a prefix older
    /// than that link). Neither channel outranks the other, so the prefix proves nothing.
    ChannelsDisagree { builds: Vec<u64> },
    /// The program is live on `PATH` but NO `current` link resolves into it: it was shimmed
    /// without ever being activated, or its `store/<program>/current` dangles because the
    /// build it named was removed. (A prefix last written by a manager older than the
    /// per-program link lands here too, for every program but the last one activated; the
    /// next `atpkg update` writes the link and clears it.) Reported rather than swallowed,
    /// because the cost is silent: that program's superseded builds are never reclaimed and
    /// the disk grows with no explanation anyone can find.
    NoLiveWitness { shims_say: u64 },
}

/// A program GC refused to consider, and why. Carries the evidence from BOTH views so the
/// report names the actual disagreement instead of "skipped".
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Divergence {
    pub program: String,
    pub reason: Diverged,
}

/// The prefix's reconciled answer to "which build of each program is live": the witnesses it
/// could prove, plus the programs it could not and why.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct LiveSet {
    live: BTreeMap<String, LiveBuild>,
    diverged: Vec<Divergence>,
}

impl LiveSet {
    /// The witness for `program`, if the prefix proved one.
    #[must_use]
    pub fn get(&self, program: &str) -> Option<&LiveBuild> {
        self.live.get(program)
    }

    /// The programs with no witness, ascending by name.
    #[must_use]
    pub fn diverged(&self) -> &[Divergence] {
        &self.diverged
    }

    /// Take the divergences, for folding into a [`GcReport`].
    #[must_use]
    pub fn into_diverged(self) -> Vec<Divergence> {
        self.diverged
    }

    /// How many programs have a proven live build.
    #[must_use]
    pub fn len(&self) -> usize {
        self.live.len()
    }

    /// Whether nothing could be proven live.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.live.is_empty()
    }
}

/// Why a reclaim was refused. Returned rather than panicked: an inconsistency has to become a
/// report line, not an abort in the middle of a best-effort maintenance pass.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Retained {
    /// The build IS the live one. Unreachable from [`run`] (whose candidates come from
    /// [`reclaimable`], which never returns the witness's own build) — this is the check that
    /// makes that a property of the code rather than of the caller's care.
    IsLive { program: String, build: u64 },
}

/// Resolve one `current` symlink into the `(program, build)` it PROVES live, or `None` if it
/// proves nothing.
///
/// Fail-closed on every axis, because a false witness is worse than none:
///
/// * **Containment by stripping, not by searching.** [`crate::ops::store_build_of`] strips
///   `<prefix>/store` instead of searching for a component named `store`, so a link pointing
///   anywhere else with a `store/<name>/<n>` tail is not ours, and `..` is rejected for free
///   (a `ParentDir` component is not `Normal`).
/// * **Exactly a build dir, no tail.** Activation points these links at
///   `store/<program>/<build>/` and nothing deeper; requiring exactly two components refuses
///   any other shape rather than parsing a prefix of it.
/// * **A dangling link proves nothing.** There is no live tree to protect, and treating the
///   number as live would let the rollback rule delete a build that IS in use.
fn current_target(prefix: &Path, current: &Path) -> Option<(String, u64)> {
    // read_link, NOT platform::resolve_shim: `current` is a directory symlink on Unix and a
    // directory JUNCTION on Windows, both of which std reads back; resolve_shim's Windows
    // half parses a `.cmd` wrapper and would return None here. Matches `ops::uninstall`'s
    // channel sweep, which is the only other reader of these links.
    let target = std::fs::read_link(current).ok()?;
    if target
        .strip_prefix(prefix.join("store"))
        .ok()?
        .components()
        .count()
        != 2
    {
        return None;
    }
    if !target.is_dir() {
        return None;
    }
    crate::ops::store_build_of(prefix, &target)
}

/// Every build the AUTHORITATIVE `current` links select, keyed by program: the per-program
/// `store/<program>/current` where it answers, and `channels/<c>/current` only for the
/// programs it does not.
///
/// A program mapping to more than one build is contested and gets no witness. Unreadable
/// directories yield an empty map — no witnesses at all, which is the fail-closed direction:
/// GC then reclaims nothing.
///
/// **Preference, not union.** The per-program link is the authority (see the module doc: the
/// channel link cannot answer per program), and unioning the two families instead would
/// re-create the abstain-forever bug from the other side: a program activated on `beta`
/// leaves the older `channels/stable/current` behind still naming its previous build, the two
/// families "disagree", and GC skips a program whose own link says plainly which build is
/// live. Channel links are therefore a MIGRATION path only — read for a program with no link
/// of its own, i.e. a prefix last written before per-program links existed, which is
/// self-limiting because that program's next activation writes one.
///
/// Preferring a link that could itself be stale is safe because it is not the only check: if
/// an older manager binary re-activated a program without updating its per-program link, that
/// activation also rewrote the `bin/` shims, so [`live_builds`] sees a channel/shim mismatch
/// and still refuses a witness.
fn authority_claims(layout: &Layout) -> BTreeMap<String, BTreeSet<u64>> {
    let mut out: BTreeMap<String, BTreeSet<u64>> = BTreeMap::new();
    // Programs whose `store/<p>/current` link EXISTS — resolved or not. The legacy channel
    // fallback below is suppressed by existence, not by resolution: a dangling or
    // out-of-store per-program link means "this prefix has the link discipline but this
    // program's link is broken", and the honest answer is NO witness (GC abstains until the
    // next activation rewrites it) — not "quietly hand authority back to a channel link that
    // may name an older build". Keying suppression on resolution was the gap: a broken own
    // link plus one stale channel link would have minted a witness for the wrong build.
    let mut has_own_link: BTreeSet<String> = BTreeSet::new();
    if let Ok(programs) = std::fs::read_dir(layout.prefix.join("store")) {
        for p in programs.flatten() {
            if std::fs::symlink_metadata(p.path().join("current")).is_ok()
                && let Some(name) = p.file_name().to_str()
            {
                has_own_link.insert(name.to_string());
            }
            let Some((program, build)) = current_target(&layout.prefix, &p.path().join("current"))
            else {
                continue;
            };
            // The link lives in `store/<dir>/` and must resolve into `store/<dir>/` — a
            // hand-made `store/ay/current -> store/ny/7` claims nothing about either.
            if p.file_name() == std::ffi::OsStr::new(&program) {
                out.entry(program).or_default().insert(build);
            }
        }
    }
    if let Ok(channels) = std::fs::read_dir(layout.prefix.join("channels")) {
        for ch in channels.flatten() {
            if let Some((program, build)) =
                current_target(&layout.prefix, &ch.path().join("current"))
                && !has_own_link.contains(&program)
            {
                out.entry(program).or_default().insert(build);
            }
        }
    }
    out
}

/// Every build the DERIVED `bin/` shims point into, keyed by program. Unlike
/// [`crate::ops::active_builds`] this keeps the whole set instead of folding it with
/// last-write-wins, so a program whose tools disagree is *visibly* contested rather than
/// silently resolved by `read_dir` order. Used only to corroborate or contradict a channel
/// claim — never as a witness in its own right.
fn shim_claims(layout: &Layout) -> BTreeMap<String, BTreeSet<u64>> {
    let mut out: BTreeMap<String, BTreeSet<u64>> = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(layout.bin_dir()) else {
        return out;
    };
    for e in entries.flatten() {
        // resolve_shim, not read_link: on Windows a shim is a `.cmd` regular file that
        // read_link Errs on, so a raw read_link would see no shims and report every program
        // as uncontested — a fail-OPEN answer on exactly one platform.
        // store_build_of, not program_build_of_target: the latter is unanchored, so a
        // dev-link into a checkout carrying a `store/ay/18/` tail would fabricate a claim
        // about OUR ay@18 and contest a witness that is in fact uncontested.
        if let Some(target) = crate::platform::resolve_shim(&e.path())
            && let Some((program, build)) = crate::ops::store_build_of(&layout.prefix, &target)
        {
            out.entry(program).or_default().insert(build);
        }
    }
    out
}

/// Reconcile the authoritative and derived views into witnesses plus divergences.
///
/// A witness needs an authority to speak and the shims not to contradict it. Note the
/// asymmetry: shims that are SILENT (every tool tombstoned, or an empty `exposes`) do not
/// block a witness — activation is the authority and nothing on `PATH` points elsewhere —
/// but shims that DISAGREE do, because whatever the authority says, the disagreeing shim is
/// what the user's next command executes.
#[must_use]
pub fn live_builds(layout: &Layout) -> LiveSet {
    live_from_claims(&authority_claims(layout), &shim_claims(layout))
}

/// [`live_builds`]' reconciliation over claim views already read.
///
/// Split out so a caller needing the claim union too ([`run_keeping_pinned_partials`], whose
/// sweep guard is that union) reads `store/`, `channels/` and every `bin/` shim once per pass
/// instead of twice. Pure: it reads nothing and decides nothing about what may be deleted.
fn live_from_claims(
    authority: &BTreeMap<String, BTreeSet<u64>>,
    shims: &BTreeMap<String, BTreeSet<u64>>,
) -> LiveSet {
    let mut set = LiveSet::default();
    let empty = BTreeSet::new();
    let programs: BTreeSet<&String> = authority.keys().chain(shims.keys()).collect();
    for program in programs {
        let ch = authority.get(program).unwrap_or(&empty);
        let sh = shims.get(program).unwrap_or(&empty);
        let reason = if ch.len() > 1 {
            Diverged::ChannelsDisagree {
                builds: ch.iter().copied().collect(),
            }
        } else if sh.len() > 1 {
            Diverged::ShimsDisagree {
                builds: sh.iter().copied().collect(),
            }
        } else {
            match (ch.first().copied(), sh.first().copied()) {
                (Some(c), Some(s)) if c != s => Diverged::ChannelShimMismatch {
                    channel_says: c,
                    shims_say: s,
                },
                (Some(c), _) => {
                    set.live.insert(
                        program.clone(),
                        LiveBuild {
                            program: program.clone(),
                            build: c,
                        },
                    );
                    continue;
                }
                (None, Some(s)) => Diverged::NoLiveWitness { shims_say: s },
                // Unreachable: `program` came from one of the two maps, so one set is
                // non-empty. Skipping rather than asserting keeps a best-effort pass
                // best-effort.
                (None, None) => continue,
            }
        };
        set.diverged.push(Divergence {
            program: program.clone(),
            reason,
        });
    }
    set
}

/// The store builds (of one program) that may be reclaimed, given all `installed` builds and
/// the program's live witness.
///
/// Kept: the live build and the highest installed build **below** it (the rollback target).
/// Returned (reclaimable): everything else, ascending — including any build ABOVE the live
/// one, which is a staged-but-never-activated tree.
///
/// Taking `&LiveBuild` rather than `current: u64` is the point: the old signature accepted
/// any number a caller happened to have, and the number the caller happened to have came
/// from a `read_dir` fold.
#[must_use]
pub fn reclaimable(installed: &[u64], live: &LiveBuild) -> Vec<u64> {
    let mut keep: BTreeSet<u64> = BTreeSet::new();
    keep.insert(live.build);
    // The rollback target is the most-recent build the live one superseded.
    if let Some(rollback) = installed.iter().copied().filter(|&b| b < live.build).max() {
        keep.insert(rollback);
    }
    let mut out: Vec<u64> = installed
        .iter()
        .copied()
        .filter(|b| !keep.contains(b))
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Reclaim a build of the witnessed program that the caller has determined is superseded.
///
/// The only reclaim reachable from the GC decision: `store::discard_build` is
/// `pub(crate)`, so the `remove_dir_all` cannot be *written* without first obtaining a
/// [`LiveBuild`] — and that can only come from [`live_builds`]. The path is derived from the
/// witness's own program, so the call cannot be aimed at a different program's tree either.
///
/// It does NOT re-derive supersession, and the name is the caller's promise rather than this
/// function's check: the one thing refused is `build == live.build`. Pass the rollback target
/// and it is deleted — [`reclaimable`] is what keeps that from happening, and any new caller
/// must take its candidates from there rather than reading this refusal as the retention rule.
pub fn discard_superseded(layout: &Layout, live: &LiveBuild, build: u64) -> Result<(), Retained> {
    if build == live.build {
        return Err(Retained::IsLive {
            program: live.program.clone(),
            build,
        });
    }
    crate::store::discard_build(&layout.build_dir(&live.program, build));
    Ok(())
}

/// What an INTERRUPTED install left in `store/<program>/`, keyed apart because the two kinds
/// are not the same claim about disk: a partial tree is named by a build number, its stage
/// scratch only by a directory name.
#[derive(Default)]
struct Debris {
    /// `(program, build, dir)` — numeric build dirs with no completeness marker.
    partial: Vec<(String, u64, PathBuf)>,
    /// `(program, dir name, dir)` — `<build>.incoming-<pid>` / `<build>.superseded-<pid>`.
    /// Reported by NAME, not by number: the scratch of build 19 is not build 19, and saying
    /// "swept build 19" of a live 19 whose stage was killed would be a lie about the tree the
    /// user is running.
    scratch: Vec<(String, String, PathBuf)>,
    /// `<build>.vendor` / `<build>.shim-env` files whose `<build>` is gone: a record of a
    /// tree that no longer exists, which only a later build of that number could misread.
    orphan_sidecars: Vec<PathBuf>,
}

/// The sidecars swept once their build is gone ([`crate::store::STAGE_SIDECAR_SUFFIXES`]).
fn orphan_sidecar_build(name: &str) -> Option<u64> {
    crate::store::STAGE_SIDECAR_SUFFIXES
        .iter()
        .find_map(|suffix| name.strip_suffix(suffix))
        .and_then(crate::store::parse_build_name)
}

/// Whether `name` is stage scratch this manager produced: `<build>.incoming-<pid>` or
/// `<build>.superseded-<pid>`, where `<build>` is a real build number.
///
/// It matches the PRODUCER's shape rather than merely containing the marker anywhere. This
/// is an unguarded `remove_dir_all` inside a directory the user can also put things in, so
/// "looks like something we made" is not a good enough test — `notes.incoming-drafts/` is
/// not ours to delete.
///
/// Delegated to [`crate::store::stage_scratch_of`], the ONE recogniser, shared with the
/// producer's own sweep. The two used to carry independent tests and the producer's was the
/// looser of the pair, so the promise written down here was not the promise the store
/// actually kept: a `18.incoming-drafts/` GC refused was deleted by the next stage of 18.
fn is_stage_scratch(name: &str) -> bool {
    crate::store::stage_scratch_of(name).is_some()
}

/// Move back any tree a swap was killed midway through, BEFORE the sweep below decides what
/// is debris. `store/<p>/<n>` absent with exactly one `<n>.superseded-<pid>` beside it is
/// the two-rename window in [`crate::install::verify_and_stage`], and the tree parked there
/// is the user's only copy — sweeping it as scratch is how a survivable crash became a
/// deleted toolchain. See [`crate::store::recover_interrupted_swap`] for why the test is
/// this narrow and why the recovered tree is left unmarked.
///
/// A recovered tree is unmarked, so the partial arm below re-examines it under the CLAIM
/// guard — kept when a `current` link or a shim points into it (the live case this exists
/// for), swept as an ordinary orphan when nothing does. That is a strictly better question
/// than the scratch arm's, which asks nothing at all.
///
/// Returns the windows it could not close: `(program, build)` for every candidate left with
/// `<build>` absent, because recovery declined to guess between two siblings or its rename
/// failed. While nothing stands at `<build>`, its superseded sibling is the only copy of that
/// build there is, and [`interrupted_debris`] must not read it as scratch.
///
/// Measured here, not re-derived at the scan: by then this pass's own reclaim loop has
/// deleted the builds it retired, so "`<build>` is absent" would also be true of a stale
/// sibling of a build reclaimed seconds ago — genuine leftover. Answered first, absence
/// means the swap window and nothing else.
fn recover_interrupted_swaps(layout: &Layout) -> BTreeMap<String, BTreeSet<u64>> {
    let mut parked: BTreeMap<String, BTreeSet<u64>> = BTreeMap::new();
    let Ok(programs) = std::fs::read_dir(layout.prefix.join("store")) else {
        return parked;
    };
    for prog in programs.flatten() {
        let Some(program) = prog.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Ok(entries) = std::fs::read_dir(prog.path()) else {
            continue;
        };
        // Collect the build numbers with superseded scratch first: recovery mutates the
        // directory, and re-reading it mid-iteration is undefined across platforms.
        let mut candidates: BTreeSet<u64> = BTreeSet::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            if let Some(name) = name.to_str()
                && let Some((build, crate::store::Scratch::Superseded)) =
                    crate::store::stage_scratch_of(name)
            {
                candidates.insert(build);
            }
        }
        for build in candidates {
            // Manual (byte-identical) render of `format!("{build}")`: Trust-gate lowering
            // workaround — see `lib.rs::dec_u64`.
            let dir = prog.path().join(crate::dec_u64(build));
            // `crate::store::sweep_stage_scratch`'s `parked_only_copy`, asked here: the
            // recovery did not put a tree back AND nothing stands at `<build>`.
            if !crate::store::recover_interrupted_swap(&dir)
                && std::fs::symlink_metadata(&dir).is_err()
            {
                parked.entry(program.clone()).or_default().insert(build);
            }
        }
    }
    parked
}

/// Everything an install killed mid-extract leaves behind, across ALL programs.
///
/// This debris is invisible to the rest of the manager by design: [`crate::ops::list_installed`]
/// counts only marker-bearing numeric dirs and GC reclaims only what it returns, so a
/// half-extracted toolchain leaked gigabytes forever while `atpkg gc` printed "nothing to
/// reclaim". `store::sweep_stage_scratch` covers the scratch half, but only for the one build a
/// later stage happens to re-stage — once the channel pins a higher build, nothing names the
/// old one again and its scratch sits there for good.
///
/// **Guarded on CLAIMS, never on the missing marker alone.** A marker-less tree is not
/// evidence of a dead tree: `doctor` reports "active {program} build {n} store
/// missing/incomplete" precisely because a LIVE build can lose its marker, so sweeping on the
/// marker would delete the running toolchain — the bricking class this whole module exists to
/// forbid. `claimed` is the union of [`authority_claims`] and [`shim_claims`], i.e. every
/// build any `current` link or any `bin/` shim resolves into; a claimed build is skipped
/// however partial it looks, and the honest report of that state is the divergence `doctor`
/// already prints. Deliberately NOT gated on a [`LiveBuild`] witness: an interrupted *fresh*
/// install has no live build at all, which is exactly the case that leaks.
///
/// Scratch needs no claim guard: both views parse `store/<program>/<n>` and nothing else, so
/// no link and no shim can name a `<build>.incoming-<pid>`, and the store-wide writer lock
/// means the stager that owned any scratch here is gone. Gone is not quiet, though — the
/// staging lane's extractor is a launchd job that outlives its submitter — so the sweep
/// below stops those orphans first.
///
/// One guard of its own: a `<build>.superseded-<pid>` whose `<build>` is absent is not
/// debris but the only copy of that build on disk, a swap window
/// [`recover_interrupted_swaps`] could not close — the `parked` pairs.
fn interrupted_debris(
    layout: &Layout,
    claimed: &BTreeMap<String, BTreeSet<u64>>,
    parked: &BTreeMap<String, BTreeSet<u64>>,
    in_scope: &dyn Fn(&str) -> bool,
) -> Debris {
    let mut out = Debris::default();
    let Ok(programs) = std::fs::read_dir(layout.prefix.join("store")) else {
        return out;
    };
    for prog in programs.flatten() {
        let Some(program) = prog.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if !in_scope(&program) {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(prog.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            // A sidecar whose build is PROVABLY absent: nothing it describes exists. A build
            // parked mid-swap is absent too, but its sidecars belong to the tree parked
            // beside it and stay.
            if entry.file_type().is_ok_and(|t| t.is_file())
                && let Some(build) = entry.file_name().to_str().and_then(orphan_sidecar_build)
                && !parked.get(&program).is_some_and(|b| b.contains(&build))
                && matches!(
                    crate::store::presence(&prog.path().join(crate::dec_u64(build))),
                    crate::store::Presence::Absent
                )
            {
                out.orphan_sidecars.push(entry.path());
                continue;
            }
            // Directories only. `DirEntry::file_type` does not follow symlinks, which is what
            // we want: `current` is a symlink (a junction on Windows) and is excluded here as
            // well as by the name test below, and `<n>.ready` / `<n>.provenance` /
            // `<n>.shim-env` are files.
            if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            // The store's own strict reader ([`crate::store::parse_build_name`]): `+18/` and
            // `018/` are not builds this manager ever wrote, and this arm sweeps what it
            // accepts. A name that is neither a build nor scratch falls through both arms.
            if let Some(build) = crate::store::parse_build_name(name) {
                if crate::store::build_is_complete(&entry.path()) {
                    continue; // an installed build — `reclaimable` owns it
                }
                if crate::store::build_marks_another_slice(&entry.path()) {
                    // A complete install of the other slice of the universal binary.
                    // `build_is_complete` refuses it on purpose so this slice re-stages it,
                    // but that refusal is about which compiler may run, not about whether an
                    // install ever finished — and nothing claims a rollback tree, so reading
                    // it as debris deletes a whole installed toolchain.
                    continue;
                }
                if claimed.get(&program).is_some_and(|s| s.contains(&build)) {
                    continue; // something on disk points into it: not ours to delete
                }
                out.partial.push((program.clone(), build, entry.path()));
            } else if is_stage_scratch(name) {
                // Not debris: a `<build>.superseded-<pid>` with nothing at `<build>` is the
                // only copy of that build on disk — a swap window
                // [`recover_interrupted_swaps`] could not close. Still swept:
                // `.incoming-<pid>` half-extracts, and superseded siblings of a live build.
                if let Some((build, crate::store::Scratch::Superseded)) =
                    crate::store::stage_scratch_of(name)
                    && parked.get(&program).is_some_and(|b| b.contains(&build))
                {
                    continue;
                }
                out.scratch
                    .push((program.clone(), name.to_string(), entry.path()));
            }
        }
    }
    // `read_dir` order is arbitrary; the report is a user-facing list.
    out.partial.sort();
    out.scratch.sort();
    out
}

/// What a GC pass did: what it reclaimed, what it swept, and what it refused to touch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GcReport {
    /// `(program, discarded builds ascending)` for every program that had reclaimable builds.
    pub reclaimed: Vec<(String, Vec<u64>)>,
    /// `(program, swept builds ascending)` for marker-less trees an interrupted install left
    /// behind. Deliberately NOT folded into `reclaimed`: these were never installed, so
    /// attributing their bytes to an ordinary reclaim would misreport where the space went —
    /// and the leak, which was silent for as long as it existed, stays visible.
    pub swept_partial: Vec<(String, Vec<u64>)>,
    /// `(program, swept scratch dir names ascending)` — the `<build>.incoming-<pid>` /
    /// `<build>.superseded-<pid>` siblings of a stage that was killed between its extract and
    /// its swap.
    pub swept_scratch: Vec<(String, Vec<String>)>,
    /// Programs skipped because the prefix could not prove which of their builds is live.
    /// GC never resolves a disagreement by deleting something; `atpkg doctor` prints these,
    /// so a divergence surfaces as a diagnostic instead of as unexplained disk growth.
    pub diverged: Vec<Divergence>,
    /// `(program, staged file names ascending)` for compressed archives a killed
    /// download left in `staging/`.
    ///
    /// Its own category because it is its own leak: `swept_partial`/`swept_scratch`
    /// are extracted trees under `store/`, while this is the multi-hundred-MB
    /// `.tar.zst` that sits in `staging/<program>/` for the whole
    /// download+verify+extract window. Nothing swept it — gc walked `store/` only —
    /// so a SIGKILL mid-install stranded the archive permanently while `atpkg gc`
    /// reported "nothing to reclaim". The batteries-included seed made that routine
    /// rather than rare: every first run parks an archive there for minutes by design.
    pub swept_staging: Vec<(String, Vec<String>)>,
    /// Exec roots (`<prefix>/compat/trust/<n>`, [`crate::compat`]) removed because their
    /// build is gone or no longer needs one, and the dot debris a killed rebuild left.
    /// Its own category for the reason `swept_staging` is: a stray root is a clone that
    /// keeps a reclaimed build's blocks allocated, outside `store/`.
    pub swept_exec_roots: Vec<PathBuf>,
    /// What the exec-root sweep could not remove, one sentence each.
    pub exec_root_errors: Vec<String>,
}

/// Reclaim superseded builds per program: the live build + one rollback are kept, the rest
/// discarded. Runs after a successful activate and behind `atpkg gc`.
///
/// A program with **no live witness is SKIPPED** — there is no safe build to compute a
/// rollback target from, and guessing is what deleted live trees. That covers a program that
/// was never activated, one whose `current` link is dangling or points out of the store, and
/// one whose two views disagree; the last three land in `diverged`. Deletion stays inside the
/// vetted prefix and never touches a live, staged, or rollback tree, so no `tree_root` is
/// perturbed and no shim is removed.
///
/// The sweep of interrupted-install debris (`interrupted_debris`) runs for EVERY program,
/// witnessed or not — a fresh install killed mid-extract never got as far as a live build, so
/// gating it on a witness would leak precisely the case it exists for. Its guard is the claim
/// union rather than the witness; see the module docs.
///
/// This standalone form spares NO staging partials — the user asked for the disk back, and
/// with no resolved index in hand there is no honest way to tell a live pinned `.part` from
/// a superseded one. An install/update pass, which DOES hold that knowledge, ends with
/// [`run_keeping_pinned_partials`] instead so a failed member's resume state survives to the
/// next pass.
#[must_use]
pub fn run(layout: &Layout) -> GcReport {
    run_keeping_pinned_partials(layout, &|_| None)
}

/// [`run`], with the staging sweep sparing — per program — the `.part` AND the complete
/// archive whose asset name matches the program's CURRENT pinned artifact (`pinned_asset`
/// maps a program name to that asset file name, from the caller's resolved signed index;
/// `None` spares nothing for that program).
///
/// Why the `.part`: gc runs at the end of every install/update pass, and the staging sweep
/// used to remove EVERY regular file under `staging/<program>/` — `.part` included — so a
/// program whose download failed mid-pass lost its resume state to that same pass's closing
/// gc and the next pass refetched from byte 0. Resume-across-passes survived only a process
/// kill.
///
/// Why the complete `<asset>`: the same guarantee, for a member that did NOT fail. A
/// coherence group aborts all-or-nothing and discards every staged sibling's extracted
/// tree, so without this a tuple that failed on its third member re-downloaded the first
/// two — the biggest of them a multi-gigabyte toolchain — on every retry, and a flaky link
/// could keep it from ever converging. `flow`'s group transaction keeps a staged member's
/// verified archive when the tuple aborts, and the next pass re-stages from it after
/// re-checking the signed sha256 + `tree_root` ([`crate::flow::carried_archive`]); this is
/// the sweep agreeing not to undo that between the two passes.
///
/// The keep rule is `flow::sweep_foreign_partials`' at-most-one bound, expressed as exact
/// file-name matches (a directory holds at most one file of each name, so "at most one
/// spared archive and one spared partial per program" holds by construction). Superseded
/// builds' archives and partials are still swept, and everything spared here remains
/// reclaimable by the standalone [`run`] — neither a killed download nor an aborted tuple
/// ever permanently strands bytes.
///
/// Trust posture: `pinned_asset` only ever SHRINKS the sweep, never extends it — it can
/// keep a file the pass was about to finish, but cannot make gc delete anything the plain
/// sweep would not, follow a symlink (the regular-file check runs first), or touch other
/// programs' directories.
#[must_use]
pub fn run_keeping_pinned_partials(
    layout: &Layout,
    pinned_asset: &dyn Fn(&str) -> Option<String>,
) -> GcReport {
    run_with(layout, pinned_asset, &running_executables)
}

/// [`run_keeping_pinned_partials`] confined to `programs`: only their superseded builds
/// are reclaimed, only their interrupted-install debris and orphan sidecars swept, and
/// only their `staging/<program>/` files — less `pinned_asset`'s — removed. Every other
/// program's builds, `.part` resume state and carried archive are left exactly as they
/// are, and so are the exec roots (the trust toolchain's). A lane that resolved only
/// some programs — the vendor lane's doors, which know nothing of the index lane's
/// pins — ends with this form, never the whole-prefix one.
#[must_use]
pub fn run_for_programs(
    layout: &Layout,
    programs: &BTreeSet<String>,
    pinned_asset: &dyn Fn(&str) -> Option<String>,
) -> GcReport {
    run_in(
        layout,
        Some(&|p: &str| programs.contains(p)),
        pinned_asset,
        &running_executables,
    )
}

/// [`run_keeping_pinned_partials`] with the process enumeration injected: `running`
/// answers the executable paths of the processes alive now, or `None` when they cannot be
/// enumerated ([`InUse`]).
fn run_with(
    layout: &Layout,
    pinned_asset: &dyn Fn(&str) -> Option<String>,
    running: &dyn Fn() -> Option<Vec<PathBuf>>,
) -> GcReport {
    run_in(layout, None, pinned_asset, running)
}

/// [`run_with`] over the programs `scope` admits (`None`: the whole prefix, exec roots
/// included).
fn run_in(
    layout: &Layout,
    scope: Option<&dyn Fn(&str) -> bool>,
    pinned_asset: &dyn Fn(&str) -> Option<String>,
    running: &dyn Fn() -> Option<Vec<PathBuf>>,
) -> GcReport {
    let in_scope = |program: &str| scope.is_none_or(|admits| admits(program));
    let mut in_use = InUse {
        layout,
        enumerate: running,
        running: None,
    };
    // First, before any view is computed and before anything is deleted: put back any tree a
    // swap was killed midway through, so the claim views see a `current` link that resolves.
    // What it could not put back it names, and the debris scan spares those siblings.
    let parked = recover_interrupted_swaps(layout);
    // The two claim views, read once for both answers below: the witness needs them to agree,
    // the sweep guard needs their union. Reading them twice costs a second walk of `store/`
    // and `channels/` and a second resolve of every shim, on a pass that runs after every
    // install and update as well as behind `atpkg gc`.
    let authority = authority_claims(layout);
    let shims = shim_claims(layout);
    let live = live_from_claims(&authority, &shims);
    // The two views UNIONED, which is the right shape here and the wrong shape for a witness:
    // `live_builds` needs them to agree, the sweep only needs to know that SOMETHING points
    // into a build. A contested build has no witness but is still one the user's next command
    // executes, so it must survive.
    let mut claimed = authority;
    for (program, builds) in shims {
        claimed.entry(program).or_default().extend(builds);
    }
    let mut by_prog: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    for (p, b) in crate::ops::list_installed(layout) {
        by_prog.entry(p).or_default().push(b);
    }
    let mut reclaimed = Vec::new();
    for (program, installed) in by_prog {
        if !in_scope(&program) {
            continue;
        }
        let Some(witness) = live.get(&program) else {
            continue;
        };
        let mut gone = Vec::new();
        for b in reclaimable(&installed, witness) {
            if in_use.holds(&program, &layout.build_dir(&program, b), Some(b)) {
                continue;
            }
            // `Retained::IsLive` cannot fire here — `reclaimable` never yields the witness's
            // own build — but it is honoured rather than unwrapped so that a future change to
            // the retention rule loses a reclaim, not the user's toolchain.
            if discard_superseded(layout, witness, b).is_ok() {
                gone.push(b);
            }
        }
        if !gone.is_empty() {
            reclaimed.push((program, gone));
        }
    }

    let debris = interrupted_debris(layout, &claimed, &parked, &in_scope);
    let mut swept_partial: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    for (program, build, path) in debris.partial {
        if in_use.holds(&program, &path, Some(build)) {
            continue;
        }
        // `store::discard_build`, not `discard_superseded`: a partial tree belongs to no
        // witness (an interrupted fresh install has no live build to produce one), and the
        // claim guard is already strictly stronger evidence of not-live than supersession —
        // nothing on disk points at this tree at all. `discard_build` also takes the sibling
        // `<n>.ready` / `<n>.provenance` / `<n>.shim-env`, so no stale sidecar outlives the
        // tree and mis-marks a later reinstall of the same build number.
        crate::store::discard_build(&path);
        // Reported only when the tree actually went away — the same contract as the
        // scratch arm below. `discard_build` is best-effort and returns nothing, so
        // existence afterwards is the only signal there is; without this check an
        // EACCES/EROFS/EBUSY on the tree prints "swept" on every pass, forever, over a
        // directory that is still there.
        if !path.exists() {
            swept_partial.entry(program).or_default().push(build);
        }
    }
    for sidecar in debris.orphan_sidecars {
        let _ = std::fs::remove_file(sidecar);
    }
    let mut swept_scratch: BTreeMap<String, Vec<String>> = BTreeMap::new();
    // A dead stager's launchd-parented helper is still extracting into its scratch; stop it
    // and wait for it before this loop deletes the directory under it. A scoped pass with
    // no scratch of its programs to delete has no helper to stop.
    if scope.is_none() || !debris.scratch.is_empty() {
        crate::stage_helper::stop_orphaned_lane_jobs();
    }
    for (program, name, path) in debris.scratch {
        // Reported only when the removal actually happened: the point of the line is to say
        // where the disk went, and a scratch dir we failed to unlink (a permissions problem,
        // a busy handle) did not go anywhere. The next pass enumerates it again.
        if std::fs::remove_dir_all(&path).is_ok() {
            swept_scratch.entry(program).or_default().push(name);
        }
    }

    // Sweep `staging/`. Safe by the same argument the scratch sweep uses: this runs
    // under the store-wide writer lock, so any archive still sitting here is owned by
    // a process that no longer exists — a live install holds the lock and would not
    // let us in. Removing a file the CURRENT pass is about to write is impossible for
    // the same reason.
    let mut swept_staging: Vec<(String, Vec<String>)> = Vec::new();
    if let Ok(programs) = std::fs::read_dir(layout.prefix.join("staging")) {
        for program in programs.filter_map(Result::ok) {
            let Ok(name) = program.file_name().into_string() else {
                continue;
            };
            if !in_scope(&name) {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(program.path()) else {
                continue;
            };
            // The files this program's sweep spares, both keyed to its CURRENT pinned
            // asset: `<asset>.part`, the resume state an interrupted transfer left, and
            // `<asset>` itself — the complete, signature-verified archive an aborted
            // coherence group carried past its stage so the next pass can re-stage that
            // member without re-downloading it (`flow::carried_archive`). Exact-name
            // matches, so at most one file of each name per program can ever be spared.
            let pinned = pinned_asset(&name);
            let keep_part = pinned.as_ref().map(|asset| format!("{asset}.part"));
            let keep_archive = pinned;
            let mut gone: Vec<String> = Vec::new();
            for e in entries.filter_map(Result::ok) {
                // Regular files only: never follow a symlink out of the prefix, and
                // never recurse into something that is not ours to interpret.
                if !e.file_type().is_ok_and(|t| t.is_file()) {
                    continue;
                }
                // A non-UTF-8 name can never be a pinned asset's partial (asset names
                // come from the signed manifest, which is UTF-8), so it is swept —
                // unreported, exactly as before.
                let entry_name = e.file_name().into_string().ok();
                if entry_name.is_some() && (keep_archive == entry_name || keep_part == entry_name) {
                    continue;
                }
                if std::fs::remove_file(e.path()).is_ok()
                    && let Some(n) = entry_name
                {
                    gone.push(n);
                }
            }
            if !gone.is_empty() {
                gone.sort();
                swept_staging.push((name, gone));
            }
        }
        swept_staging.sort();
    }

    // LAST, over the store as this pass left it: the exec roots (`crate::compat`). Every
    // discard above already took its build's root through `store::discard_build`; this
    // is for the root that outlived its build some other way — a removal that failed
    // part-way, a build an older client reclaimed, a build that stopped needing one — and
    // for the dot debris a killed rebuild left. A root holds clones of its build's
    // files, so a stray one keeps a reclaimed build's gigabytes allocated with nothing
    // under `store/` to show for them.
    let exec_roots = if scope.is_none() {
        crate::compat::sweep(layout)
    } else {
        crate::compat::Report::default()
    };

    GcReport {
        swept_staging,
        reclaimed,
        swept_partial: swept_partial.into_iter().collect(),
        swept_scratch: swept_scratch.into_iter().collect(),
        diverged: live
            .into_diverged()
            .into_iter()
            .filter(|d| in_scope(&d.program))
            .collect(),
        swept_exec_roots: exec_roots.swept,
        exec_root_errors: exec_roots.errors,
    }
}

/// Whether a live process runs from a build, asked before one is reclaimed — for EVERY
/// program. A long `codex` session execs helpers out of its own build; a `targo build`
/// spawns a fresh `trustc` per crate from the build it started on, so a superseded
/// toolchain reclaimed mid-build failed the build at its next crate (widened from the
/// agent programs, 2026-09-24). A trust build is also judged by its exec root
/// ([`crate::compat::root_dir`]), which [`crate::store::discard_build`] takes with it.
///
/// The enumeration runs at most once per pass, and only once a build is a candidate.
/// When it cannot be taken, every AGENT build is kept (the fail-safe it always had) and
/// every other build is reclaimed under the plain rule, as before — a platform with no
/// process table must not stop gc for good.
struct InUse<'a> {
    layout: &'a Layout,
    enumerate: &'a dyn Fn() -> Option<Vec<PathBuf>>,
    running: Option<Option<Vec<PathBuf>>>,
}

impl InUse<'_> {
    /// `true` when `build_dir` of `program` (build number `build`, when it is one) must be
    /// kept because a process may run from it.
    fn holds(&mut self, program: &str, build_dir: &Path, build: Option<u64>) -> bool {
        let enumerate = self.enumerate;
        let Some(running) = self.running.get_or_insert_with(enumerate) else {
            return crate::stub::is_agent_program(program);
        };
        let root = build
            .filter(|_| program == crate::seam::SEAM_PROGRAM)
            .map(|b| crate::compat::root_dir(self.layout, b));
        // Both spellings: the kernel reports resolved paths, the layout may not be one.
        let mut dirs = vec![build_dir.to_path_buf()];
        dirs.extend(root);
        let resolved: Vec<PathBuf> = dirs
            .iter()
            .filter_map(|d| std::fs::canonicalize(d).ok())
            .collect();
        if resolved.is_empty() && std::fs::symlink_metadata(build_dir).is_ok() {
            return true; // there, but not resolvable: keep it
        }
        running.iter().any(|exe| {
            dirs.iter()
                .chain(resolved.iter())
                .any(|d| exe.starts_with(d))
        })
    }
}

/// Whether a live process runs from under `dir`, by the kernel's path for each
/// process (so a relative, symlinked or PATH launch is seen, unlike `ps`'s argv[0]).
/// `None` when the process table cannot be read. Both spellings of `dir` are checked:
/// the kernel reports resolved paths.
#[must_use]
pub fn runs_from(dir: &Path) -> Option<bool> {
    running_from(dir, &running_executables).map(|exe| exe.is_some())
}

/// [`runs_from`]'s rule over an injected enumeration (`running`, answering as
/// [`running_executables`] does), naming the first executable found under `dir`:
/// `Some(None)` when none is, `None` when the table cannot be read — the unknown case is
/// the caller's to price. [`crate::seam`] asks it before it re-lays a rustup view, a
/// directory that is not a store build and so is outside [`InUse`]'s reach.
pub(crate) fn running_from(
    dir: &Path,
    running: &dyn Fn() -> Option<Vec<PathBuf>>,
) -> Option<Option<PathBuf>> {
    let running = running()?;
    let resolved = std::fs::canonicalize(dir).ok();
    Some(running.into_iter().find(|exe| {
        exe.starts_with(dir) || resolved.as_deref().is_some_and(|r| exe.starts_with(r))
    }))
}

/// The live process table ([`running_executables`]) under one ungated name, for another
/// module to hand to [`running_from`]. The three platform declarations below cover every
/// target between them, but only as a three-way split, and a `crate::`-qualified reference
/// is held to a declaration that survives wherever the reference does
/// (`tests/platform_cfg_parity.rs`).
pub(crate) fn process_table() -> Option<Vec<PathBuf>> {
    running_executables()
}

/// The executable path of every process alive now, from `proc_listallpids` +
/// `proc_pidpath`. A process that exits mid-walk, or whose executable has no path left,
/// is skipped; any other failure is `None` (unknown).
#[cfg(target_os = "macos")]
fn running_executables() -> Option<Vec<PathBuf>> {
    use std::os::unix::ffi::OsStrExt as _;
    unsafe extern "C" {
        fn proc_listallpids(buffer: *mut libc::c_void, buffersize: libc::c_int) -> libc::c_int;
        fn proc_pidpath(
            pid: libc::c_int,
            buffer: *mut libc::c_void,
            buffersize: u32,
        ) -> libc::c_int;
    }
    /// `PROC_PIDPATHINFO_MAXSIZE` — `4 * MAXPATHLEN`.
    const PATH_MAX_BYTES: usize = 4 * 1024;
    /// A pid table this long is not a Mac; refuse rather than allocate for it.
    const MAX_PIDS: usize = 1 << 20;
    // SAFETY: a null buffer asks only for the number of pids; nothing is written.
    let estimate = unsafe { proc_listallpids(std::ptr::null_mut(), 0) };
    let mut capacity = usize::try_from(estimate).ok()?.saturating_add(64);
    let pids = loop {
        if capacity > MAX_PIDS {
            return None;
        }
        let mut pids: Vec<libc::c_int> = vec![0; capacity];
        let bytes = libc::c_int::try_from(capacity * std::mem::size_of::<libc::c_int>()).ok()?;
        // SAFETY: `pids` holds exactly `bytes` bytes and outlives the call, which writes at
        // most that many and answers how many pids it wrote.
        let n = unsafe { proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
        let n = usize::try_from(n).ok().filter(|&n| n > 0)?;
        // A full table may have been cut short by processes born between the two calls.
        if n < capacity {
            pids.truncate(n);
            break pids;
        }
        capacity = capacity.saturating_mul(2);
    };
    let mut buf = vec![0u8; PATH_MAX_BYTES];
    let len = u32::try_from(buf.len()).ok()?;
    let mut out = Vec::with_capacity(pids.len());
    for pid in pids.into_iter().filter(|&pid| pid > 0) {
        // SAFETY: `buf` holds `len` bytes and outlives the call, which writes at most that.
        let written = unsafe { proc_pidpath(pid, buf.as_mut_ptr().cast(), len) };
        match usize::try_from(written) {
            Ok(n) if n > 0 => {
                let path = buf.get(..n)?;
                out.push(PathBuf::from(std::ffi::OsStr::from_bytes(path)));
            }
            _ => match std::io::Error::last_os_error().raw_os_error() {
                Some(libc::ESRCH | libc::ENOENT) => {}
                _ => return None,
            },
        }
    }
    Some(out)
}

/// The executable path of every process alive now, from `/proc/<pid>/exe` (procfs's
/// ` (deleted)` annotation removed). A process that exits mid-walk, a kernel thread and
/// another user's process are skipped; any other failure is `None` (unknown).
#[cfg(target_os = "linux")]
fn running_executables() -> Option<Vec<PathBuf>> {
    use std::os::unix::ffi::OsStrExt as _;
    let mut out = Vec::new();
    for entry in std::fs::read_dir("/proc").ok()? {
        let entry = entry.ok()?;
        let name = entry.file_name();
        if !name.as_bytes().iter().all(u8::is_ascii_digit) {
            continue;
        }
        match std::fs::read_link(entry.path().join("exe")) {
            Ok(exe) => {
                let bytes = exe.as_os_str().as_bytes();
                let bytes = bytes.strip_suffix(b" (deleted)").unwrap_or(bytes);
                out.push(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)));
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                ) => {}
            Err(_) => return None,
        }
    }
    Some(out)
}

/// No process table is read here, so whether a session runs from an agent build is
/// unknown and every agent build is kept: disk is spent before a running tree is lost.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn running_executables() -> Option<Vec<PathBuf>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A witness, for the pure retention tests. Constructible here (and only here) because
    /// `tests` is a child module of `gc`; no other module can forge one.
    fn live(program: &str, build: u64) -> LiveBuild {
        LiveBuild {
            program: program.to_string(),
            build,
        }
    }

    /// The reconciliation table, over claim views handed in rather than read off a prefix —
    /// the shape the pass uses so one gc run reads `store/`, `channels/` and `bin/` once.
    #[test]
    fn live_from_claims_reconciles_the_two_views() {
        fn claims(pairs: &[(&str, &[u64])]) -> BTreeMap<String, BTreeSet<u64>> {
            pairs
                .iter()
                .map(|(p, b)| ((*p).to_string(), b.iter().copied().collect()))
                .collect()
        }
        let build_of = |set: &LiveSet| set.live.get("ay").map(|l| l.build);

        // Agreement mints a witness…
        let set = live_from_claims(&claims(&[("ay", &[19])]), &claims(&[("ay", &[19])]));
        assert_eq!(build_of(&set), Some(19));
        assert!(set.diverged.is_empty());
        // …and so does an authority whose shims are silent: nothing on PATH points elsewhere.
        let set = live_from_claims(&claims(&[("ay", &[19])]), &claims(&[]));
        assert_eq!(build_of(&set), Some(19));
        assert!(set.diverged.is_empty());

        // Shims that contradict the authority block the witness: whatever the authority says,
        // the disagreeing shim is what the user's next command executes.
        let set = live_from_claims(&claims(&[("ay", &[19])]), &claims(&[("ay", &[18])]));
        assert!(set.live.is_empty());
        assert_eq!(set.diverged.len(), 1);
        // Two authorities, two shim targets, and a shim with no authority at all: no witness.
        for (a, sh) in [
            (claims(&[("ay", &[18, 19])]), claims(&[])),
            (claims(&[("ay", &[19])]), claims(&[("ay", &[18, 19])])),
            (claims(&[]), claims(&[("ay", &[19])])),
        ] {
            let set = live_from_claims(&a, &sh);
            assert!(set.live.is_empty(), "no witness from {a:?} / {sh:?}");
            assert_eq!(set.diverged.len(), 1);
        }
    }

    #[test]
    fn keeps_current_plus_one_rollback() {
        // live 19, rollback 18 ⇒ reclaim the older 16, 17.
        assert_eq!(
            reclaimable(&[16, 17, 18, 19], &live("ay", 19)),
            vec![16, 17]
        );
    }

    #[test]
    fn current_is_never_reclaimed() {
        assert!(reclaimable(&[19], &live("ay", 19)).is_empty());
        // Even a single installed == live, with no rollback, keeps it.
        assert!(!reclaimable(&[19], &live("ay", 19)).contains(&19));
    }

    #[test]
    fn no_rollback_below_current_keeps_only_current() {
        // live is the lowest installed ⇒ no rollback target ⇒ the higher ones (staged but
        // never activated) are reclaimable; the live build stays.
        assert_eq!(reclaimable(&[19, 20, 21], &live("ay", 19)), vec![20, 21]);
    }

    #[test]
    fn handles_duplicates_and_unsorted_input() {
        assert_eq!(
            reclaimable(&[18, 16, 19, 17, 16], &live("ay", 19)),
            vec![16, 17]
        );
    }

    // --- the imperative executor -------------------------------------------------------

    use crate::activate::{activate_channel, install_shims};
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn tool(name: &str) -> crate::store::ToolName {
        crate::store::ToolName::new(name).unwrap()
    }

    fn layout(label: &str) -> Layout {
        let p = std::env::temp_dir().join(format!("atpkg-gc-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).unwrap();
        Layout { prefix: p }
    }

    /// Lay down a COMPLETE (marker-written) build dir with `bin/<program>`. `shim` also
    /// installs the shims + activates the channel (making it the LIVE build); otherwise it
    /// is a complete but inactive build on disk.
    fn seed(layout: &Layout, program: &str, build: u64, shim: bool) -> PathBuf {
        let dir = layout.build_dir(program, build);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(
            dir.join("bin").join(tool(program).exe_file()),
            b"#!/bin/true\n",
        )
        .unwrap();
        if shim {
            install_shims(
                layout,
                &dir,
                &[program.to_string()],
                crate::activate::Aliases::Off,
            )
            .unwrap();
            activate_channel(layout, "stable", &dir).unwrap();
        }
        crate::store::mark_build_ready(&dir).unwrap();
        dir
    }

    /// A populated build dir with NO completeness marker: what an install killed between its
    /// extract and its `mark_build_ready` leaves behind. Invisible to `list_installed`, so no
    /// fixture can reach it through the ordinary reclaim path.
    fn seed_partial(layout: &Layout, program: &str, build: u64) -> PathBuf {
        let dir = layout.build_dir(program, build);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(
            dir.join("bin").join(tool(program).exe_file()),
            b"#!/bin/true\n",
        )
        .unwrap();
        assert!(
            !crate::store::build_is_complete(&dir),
            "the fixture is only interesting while it is marker-less"
        );
        dir
    }

    /// Stage scratch beside a build: the `<build>.incoming-<pid>` / `<build>.superseded-<pid>`
    /// siblings `verify_and_stage` extracts into and retires through.
    fn seed_scratch(layout: &Layout, program: &str, name: &str) -> PathBuf {
        let dir = layout.prefix.join("store").join(program).join(name);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("bin").join("payload"), b"half an extract\n").unwrap();
        dir
    }

    #[test]
    fn run_keeps_current_and_rollback_reclaims_the_rest() {
        let l = layout("run-keeps");
        for b in [16u64, 17, 18] {
            seed(&l, "ay", b, false);
        }
        seed(&l, "ay", 19, true); // ay@19 is live
        let report = run(&l);
        // Keep 19 (live) and 18 (the rollback target, highest below live).
        assert_eq!(report.reclaimed, vec![("ay".to_string(), vec![16u64, 17])]);
        assert!(report.diverged.is_empty(), "the two views agree");
        for gone in [16u64, 17] {
            assert!(!l.build_dir("ay", gone).exists(), "{gone} reclaimed");
        }
        for keep in [18u64, 19] {
            assert!(l.build_dir("ay", keep).exists(), "{keep} survives");
        }
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn run_skips_a_program_with_no_active_build() {
        let l = layout("run-noactive");
        // Complete builds on disk but NO shim and NO channel => nothing claims the program
        // at all => never reclaim, and nothing to report either (this is an ordinary
        // never-activated program, not a disagreement).
        seed(&l, "ay", 17, false);
        seed(&l, "ay", 18, false);
        let report = run(&l);
        assert!(
            report.reclaimed.is_empty(),
            "no witness => nothing reclaimed"
        );
        assert!(report.diverged.is_empty(), "unclaimed is not diverged");
        assert!(l.build_dir("ay", 17).exists());
        assert!(l.build_dir("ay", 18).exists());
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// THE regression for the bricking path. A shim left pointing at an OLDER build made
    /// `ops::active_builds` report that older build as current; `reclaimable` then classified
    /// the build the channel actually selects as superseded, and `discard_build` — which has
    /// no liveness check of its own — deleted the live tree. Now the two views disagree, so
    /// no witness exists and nothing is deleted.
    #[test]
    fn a_stale_shim_never_deletes_the_channels_live_build() {
        let l = layout("stale-shim-brick");
        seed(&l, "ay", 18, false);
        let b19 = seed(&l, "ay", 19, false);
        // The authority says 19 …
        activate_channel(&l, "stable", &b19).unwrap();
        // … but `bin/ay` still forwards into 18 (a shim-install loop that failed partway,
        // or a hand-edited prefix). `bin/` is created here because nothing in this fixture
        // called `install_shims`, which is what normally hardens it.
        crate::platform::ensure_private_dir(&l.bin_dir()).unwrap();
        let ay = tool("ay");
        crate::platform::install_shim(&l.build_dir("ay", 18).join("bin"), &ay, &l.shim(&ay))
            .unwrap();

        let report = run(&l);
        assert!(
            report.reclaimed.is_empty(),
            "a contested program is never reclaimed"
        );
        assert!(
            l.build_dir("ay", 19).exists(),
            "the channel's LIVE build must survive a stale shim"
        );
        assert!(l.build_dir("ay", 18).exists(), "and so must the shims'");
        assert_eq!(
            report.diverged,
            vec![Divergence {
                program: "ay".to_string(),
                reason: Diverged::ChannelShimMismatch {
                    channel_says: 19,
                    shims_say: 18,
                },
            }]
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// One program's tools split across two builds: the state `install_shims`' prune exists
    /// to prevent, still reachable when its per-tool loop fails before the prune runs.
    #[test]
    fn tools_of_one_program_split_across_builds_yield_no_witness() {
        let l = layout("split-shims");
        let b18 = seed(&l, "ay", 18, false);
        let b19 = seed(&l, "ay", 19, false);
        activate_channel(&l, "stable", &b19).unwrap();
        install_shims(&l, &b19, &["ay".to_string()], crate::activate::Aliases::Off).unwrap();
        // Written AFTER install_shims: the prune would otherwise remove it immediately.
        let aylint = tool("aylint");
        crate::platform::install_shim(&b18.join("bin"), &aylint, &l.shim(&aylint)).unwrap();

        let report = run(&l);
        assert!(report.reclaimed.is_empty());
        assert!(l.build_dir("ay", 18).exists() && l.build_dir("ay", 19).exists());
        assert_eq!(
            report.diverged,
            vec![Divergence {
                program: "ay".to_string(),
                reason: Diverged::ShimsDisagree {
                    builds: vec![18, 19],
                },
            }]
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// An authority that cannot be resolved gives NO witness — never a fallback to the shim
    /// view, which is the bug this module exists to close. Covers all three failure shapes:
    /// dangling, out-of-store, and unreadable link directories. Both link families are
    /// broken in each step, because either one alone still proves the claim.
    #[cfg(unix)] // dangling/out-of-store link fixtures: a Windows junction needs a real target
    #[test]
    fn a_broken_authority_skips_the_program_instead_of_reclaiming() {
        let l = layout("bad-channel");
        seed(&l, "ay", 17, false);
        seed(&l, "ay", 18, false);
        seed(&l, "ay", 19, true); // shims + both `current` links at 19
        let links = [l.channel_current("stable"), l.program_current("ay")];

        // 1. The links dangle (the build dir was removed out from under them).
        for link in &links {
            crate::activate::atomic_symlink(&l.build_dir("ay", 77), link).unwrap();
        }
        let report = run(&l);
        assert!(
            report.reclaimed.is_empty(),
            "a dangling authority reclaims nothing"
        );
        assert_eq!(
            report.diverged,
            vec![Divergence {
                program: "ay".to_string(),
                reason: Diverged::NoLiveWitness { shims_say: 19 },
            }]
        );

        // 2. The links resolve OUTSIDE store/ — a shape activation never writes.
        let outside = l.prefix.join("elsewhere").join("ay").join("19");
        std::fs::create_dir_all(&outside).unwrap();
        for link in &links {
            crate::activate::atomic_symlink(&outside, link).unwrap();
        }
        assert!(
            run(&l).reclaimed.is_empty(),
            "an out-of-store authority proves nothing"
        );

        // 3. Neither link exists at all.
        std::fs::remove_dir_all(l.prefix.join("channels")).unwrap();
        std::fs::remove_file(l.program_current("ay")).unwrap();
        assert!(run(&l).reclaimed.is_empty(), "no links => no witnesses");

        for b in [17u64, 18, 19] {
            assert!(l.build_dir("ay", b).exists(), "{b} must survive all three");
        }
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// Installed and shimmed but never activated: skipped, and REPORTED — otherwise the
    /// program is silently un-GC-able forever and the disk grows with no explanation anyone
    /// can find.
    #[test]
    fn a_program_that_was_never_activated_is_reported_diverged() {
        let l = layout("no-channel");
        seed(&l, "ay", 17, false);
        let b18 = seed(&l, "ay", 18, false);
        install_shims(&l, &b18, &["ay".to_string()], crate::activate::Aliases::Off).unwrap(); // shims, but no activate

        let report = run(&l);
        assert!(
            report.reclaimed.is_empty(),
            "no witness => nothing reclaimed"
        );
        assert!(l.build_dir("ay", 17).exists());
        assert_eq!(
            report.diverged,
            vec![Divergence {
                program: "ay".to_string(),
                reason: Diverged::NoLiveWitness { shims_say: 18 },
            }]
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// THE regression for the second bricking-adjacent bug: GC that abstains forever.
    ///
    /// Every released-tool install passes the SAME channel name, so `channels/stable/current`
    /// only ever remembers the last program activated. With that as the sole authority, `ay`
    /// here has no witness, `run` skips it, and every `atpkg update ay` adds a build that is
    /// never reclaimed — silent, unbounded growth that no verb reports. The per-program
    /// `store/<program>/current` is what makes both programs witnessable at once.
    #[test]
    fn two_programs_on_one_channel_are_both_reclaimed() {
        let l = layout("two-progs");
        for b in [16u64, 17, 18] {
            seed(&l, "ay", b, false);
        }
        seed(&l, "ay", 19, true); // ay@19 activated …
        seed(&l, "ny", 6, false);
        seed(&l, "ny", 7, true); // … then ny@7, overwriting channels/stable/current

        let report = run(&l);
        assert_eq!(
            report.reclaimed,
            vec![("ay".to_string(), vec![16u64, 17])],
            "ay must still be witnessed after ny took over the channel link"
        );
        assert!(
            report.diverged.is_empty(),
            "neither program is contested: {:?}",
            report.diverged
        );
        // ny keeps live 7 + rollback 6; ay keeps live 19 + rollback 18.
        for keep in [("ay", 18u64), ("ay", 19), ("ny", 6), ("ny", 7)] {
            assert!(
                l.build_dir(keep.0, keep.1).exists(),
                "{}@{} survives",
                keep.0,
                keep.1
            );
        }
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// Remove a `current` link whatever shape the platform gave it — a symlink (Unix) or a
    /// directory junction (Windows) — so the "prefix written before per-program links"
    /// fixtures below are not Unix-only.
    fn unlink_current(link: &Path) {
        let _ = std::fs::remove_file(link);
        let _ = std::fs::remove_dir(link);
        assert!(
            std::fs::symlink_metadata(link).is_err(),
            "{} must be gone for the pre-migration fixture",
            link.display()
        );
    }

    /// The migration half of the authority rule: a prefix last written by a manager older
    /// than the per-program link has ONLY `channels/<c>/current`, and the program it names
    /// must stay witnessed across the upgrade — otherwise adopting the new link would itself
    /// stop reclaiming until every program happened to be updated.
    #[test]
    fn a_prefix_older_than_the_per_program_link_is_witnessed_by_its_channel() {
        let l = layout("legacy-channel");
        seed(&l, "ay", 17, false);
        seed(&l, "ay", 18, false);
        seed(&l, "ay", 19, true);
        unlink_current(&l.program_current("ay")); // the pre-migration on-disk shape

        let report = run(&l);
        assert_eq!(
            report.reclaimed,
            vec![("ay".to_string(), vec![17u64])],
            "the channel link is still an authority for a program with no link of its own"
        );
        assert!(report.diverged.is_empty(), "{:?}", report.diverged);
        assert!(l.build_dir("ay", 18).exists() && l.build_dir("ay", 19).exists());
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// Preference, not union — and why it matters. A program activated on a second channel
    /// leaves the first channel's link behind naming its PREVIOUS build. Treating both
    /// families as co-equal authorities would call that a disagreement and abstain forever on
    /// a program whose own `current` says plainly which build is live: the same
    /// silent-unbounded-growth failure the per-program link was added to fix, entered from
    /// the other side. The tie is only unbreakable when the program has no link of its own.
    #[test]
    fn a_stale_other_channel_link_does_not_contest_the_programs_own_current() {
        let l = layout("stale-channel");
        seed(&l, "ay", 17, false);
        let b18 = seed(&l, "ay", 18, false);
        activate_channel(&l, "beta", &b18).unwrap(); // an earlier activation, another channel
        seed(&l, "ay", 19, true); // now: beta→18 (stale), stable→19, store/ay/current→19

        let report = run(&l);
        assert_eq!(
            report.reclaimed,
            vec![("ay".to_string(), vec![17u64])],
            "the program's own link outranks a channel link it superseded"
        );
        assert!(report.diverged.is_empty(), "{:?}", report.diverged);

        // Take that link away and the two channels are all that is left: neither outranks the
        // other, so the prefix proves nothing and GC abstains instead of picking one.
        unlink_current(&l.program_current("ay"));
        let report = run(&l);
        assert!(report.reclaimed.is_empty());
        assert_eq!(
            report.diverged,
            vec![Divergence {
                program: "ay".to_string(),
                reason: Diverged::ChannelsDisagree {
                    builds: vec![18, 19],
                },
            }]
        );
        assert!(l.build_dir("ay", 18).exists() && l.build_dir("ay", 19).exists());
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// Suppression is by link EXISTENCE, not resolution. A program whose own `current` link
    /// is present but broken gets NO witness — the channel fallback must not answer for it.
    /// If suppression keyed on resolution instead, a dangling own link would quietly hand
    /// authority back to a channel link that may name an older build, and `reclaimable`'s
    /// "above live = staged, delete it" rule would then aim at the NEWER tree. The honest
    /// answer to a broken link is abstention (`NoLiveWitness`, which `atpkg gc`/`doctor`
    /// report) until the next activation rewrites it.
    #[test]
    fn a_broken_program_link_never_hands_authority_back_to_a_channel() {
        let l = layout("broken-own-link");
        seed(&l, "ay", 17, false);
        seed(&l, "ay", 18, false);
        seed(&l, "ay", 19, true); // own→19, stable→19, shims→19

        // Break the own link portably: point it at a build dir that exists, then delete
        // the dir. (A junction may dangle on Windows exactly like a symlink on Unix.)
        let doomed = l.build_dir("ay", 99);
        std::fs::create_dir_all(&doomed).unwrap();
        unlink_current(&l.program_current("ay"));
        crate::activate::atomic_symlink(&doomed, &l.program_current("ay")).unwrap();
        std::fs::remove_dir_all(&doomed).unwrap();

        let report = run(&l);
        assert!(
            report.reclaimed.is_empty(),
            "a broken own link must abstain, not fall back to the channel: {:?}",
            report.reclaimed
        );
        assert_eq!(
            report.diverged,
            vec![Divergence {
                program: "ay".to_string(),
                reason: Diverged::NoLiveWitness { shims_say: 19 },
            }]
        );
        // Nothing was deleted on either side of the would-be witness.
        for b in [17u64, 18, 19] {
            assert!(l.build_dir("ay", b).exists(), "{b} must survive");
        }
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// The shim view is parsed with the ANCHORED containment test, so a link into a tree this
    /// manager does not own claims nothing. With the unanchored parser a dev-link into
    /// `~/src/store/ay/18/bin/aydev` reads as OUR ay@18, contests an otherwise-uncontested
    /// witness, and GC abstains on `ay` for as long as the developer keeps that link. (Its
    /// deleting twin — `activate::prune_stale_shims` must not remove such a link — is
    /// asserted in `activate.rs`.)
    #[cfg(unix)] // out-of-prefix symlink fixture
    #[test]
    fn a_shim_into_a_lookalike_store_outside_the_prefix_claims_nothing() {
        let l = layout("foreign-store-claim");
        seed(&l, "ay", 17, false);
        seed(&l, "ay", 18, false);
        seed(&l, "ay", 19, true);

        let devco = l
            .prefix
            .parent()
            .unwrap()
            .join(format!("atpkg-gc-devco-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&devco);
        let checkout = devco.join("store/ay/18/bin");
        std::fs::create_dir_all(&checkout).unwrap();
        std::fs::write(checkout.join("aydev"), b"#!/bin/true\n").unwrap();
        let aydev = tool("aydev");
        std::os::unix::fs::symlink(checkout.join("aydev"), l.shim(&aydev)).unwrap();

        let report = run(&l);
        assert_eq!(
            report.reclaimed,
            vec![("ay".to_string(), vec![17u64])],
            "a target outside <prefix>/store is not a claim about ay"
        );
        assert!(report.diverged.is_empty(), "{:?}", report.diverged);
        assert!(l.build_dir("ay", 18).exists() && l.build_dir("ay", 19).exists());
        let _ = std::fs::remove_dir_all(&devco);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    // --- interrupted-install debris ----------------------------------------------------

    /// The leak itself: `list_installed` gates on the marker and GC reclaims only what
    /// `list_installed` returns, so a tree left by an install killed mid-extract could not be
    /// named by any verb — `atpkg gc` printed "nothing to reclaim" over gigabytes of it,
    /// forever. It is swept now, and reported APART from the reclaim: those bytes were never
    /// an installed build, and calling them one would misattribute where the space went.
    #[test]
    fn an_unclaimed_partial_tree_is_swept_and_never_counted_as_a_reclaim() {
        let l = layout("sweep-partial");
        for b in [16u64, 17, 18] {
            seed(&l, "ay", b, false);
        }
        seed(&l, "ay", 19, true); // ay@19 is live
        // An update to 20 killed between extract and marker: populated, unmarked, and with
        // no link or shim into it because activation never ran.
        let partial = seed_partial(&l, "ay", 20);

        let report = run(&l);
        assert!(!partial.exists(), "the interrupted tree must be reclaimed");
        assert_eq!(report.swept_partial, vec![("ay".to_string(), vec![20u64])]);
        assert_eq!(
            report.reclaimed,
            vec![("ay".to_string(), vec![16u64, 17])],
            "an ordinary reclaim must not absorb the sweep"
        );
        assert!(report.diverged.is_empty(), "{:?}", report.diverged);
        for keep in [18u64, 19] {
            assert!(l.build_dir("ay", keep).exists(), "{keep} survives");
        }
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// The case a witness-gated sweep cannot reach at all: a FIRST install of a program,
    /// killed mid-extract. There is no live build, no shim and no channel link — `run` skips
    /// the program for every other purpose, and gating the sweep on a witness too would leak
    /// exactly the tree the sweep exists for.
    #[test]
    fn an_interrupted_fresh_install_is_swept_although_nothing_is_live() {
        let l = layout("sweep-fresh");
        let partial = seed_partial(&l, "ny", 7);

        let report = run(&l);
        assert!(
            !partial.exists(),
            "a never-activated program leaks disk too"
        );
        assert_eq!(report.swept_partial, vec![("ny".to_string(), vec![7u64])]);
        assert!(report.reclaimed.is_empty());
        assert!(
            report.diverged.is_empty(),
            "a program that never finished installing is not a disagreement"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// THE guard, from the side that would brick a prefix. The marker is a SIBLING file, so a
    /// LIVE build can lose it without losing its tree — `doctor` reports that very state
    /// ("active build store missing/incomplete"), which is what makes it reachable. Sweeping
    /// on "no marker" alone would then `remove_dir_all` the running toolchain, so the guard is
    /// the CLAIM: `store/ay/current` resolves into 19, therefore 19 stays.
    #[test]
    fn a_live_build_that_lost_its_marker_is_claimed_and_never_swept() {
        let l = layout("marker-less-live");
        seed(&l, "ay", 18, false);
        let b19 = seed(&l, "ay", 19, true);
        crate::store::clear_build_ready(&b19).unwrap();
        assert!(!crate::store::build_is_complete(&b19));

        let report = run(&l);
        assert!(
            report.swept_partial.is_empty(),
            "a claimed tree is not debris: {:?}",
            report.swept_partial
        );
        assert!(b19.exists(), "the live tree must survive losing its marker");
        assert!(l.build_dir("ay", 18).exists(), "and so must its rollback");
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// …and from the side no witness could have guarded. Here the two views DISAGREE, so
    /// there is no `LiveBuild` at all — yet `bin/ay` forwards into the marker-less 18, which
    /// is what the user's next command executes. Only the claim union (authority ∪ shims)
    /// protects it; a sweep guarded on the witness's build number would have had no witness to
    /// consult and deleted the tree on PATH.
    #[test]
    fn a_marker_less_build_the_shims_still_run_is_never_swept() {
        let l = layout("marker-less-contested");
        let b18 = seed(&l, "ay", 18, false);
        let b19 = seed(&l, "ay", 19, false);
        activate_channel(&l, "stable", &b19).unwrap(); // the authority says 19 …
        // … and `bin/ay` says 18, which is the build the user's next command runs.
        crate::platform::ensure_private_dir(&l.bin_dir()).unwrap();
        let ay = tool("ay");
        crate::platform::install_shim(&b18.join("bin"), &ay, &l.shim(&ay)).unwrap();
        crate::store::clear_build_ready(&b18).unwrap();

        let report = run(&l);
        assert!(
            report.swept_partial.is_empty(),
            "the shims' claim is a claim: {:?}",
            report.swept_partial
        );
        assert!(b18.exists(), "the tree on PATH must survive");
        assert!(b19.exists(), "and so must the one the channel selects");
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// Stage scratch outlives its stager. `store::sweep_stage_scratch` clears it only for the
    /// ONE build a later stage re-stages, so once the channel pins a higher build nothing ever
    /// names the old scratch again. It is reclaimed by NAME — a `19.superseded-<pid>` is not
    /// build 19, and reporting it as one would claim the live tree had been swept.
    #[test]
    fn stage_scratch_of_a_killed_install_is_swept_by_name_not_by_build() {
        let l = layout("sweep-scratch");
        seed(&l, "ay", 18, false);
        seed(&l, "ay", 19, true);
        let incoming = seed_scratch(&l, "ay", "20.incoming-4242");
        let superseded = seed_scratch(&l, "ay", "19.superseded-4242");

        let report = run(&l);
        assert!(!incoming.exists() && !superseded.exists(), "scratch swept");
        assert_eq!(
            report.swept_scratch,
            vec![(
                "ay".to_string(),
                vec![
                    "19.superseded-4242".to_string(),
                    "20.incoming-4242".to_string(),
                ]
            )]
        );
        assert!(report.swept_partial.is_empty(), "scratch is not a build");
        assert!(
            l.build_dir("ay", 19).exists(),
            "the live build survives the sweep of its own scratch"
        );
        assert!(l.build_dir("ay", 18).exists());
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// THE SCRATCH TEST IS A NAME TEST, AND IT DELETES DIRECTORIES. `store/<program>/` is
    /// a directory a user can also drop things into, so "contains `.incoming-`" is not a
    /// good enough licence to `remove_dir_all`: the match has to be the exact shape the
    /// producer writes (`<build>.incoming-<pid>`).
    #[test]
    fn only_the_producers_own_scratch_shape_is_sweepable() {
        for ours in [
            "18.incoming-1",
            "18.superseded-4242",
            "0.incoming-999999",
            "18446744073709551615.incoming-7",
        ] {
            assert!(is_stage_scratch(ours), "{ours} is ours");
        }
        for theirs in [
            "18",
            "18.ready",
            "notes.incoming-drafts",
            "18.incoming-",
            "18.incoming-4242x",
            "ay.incoming-4242",
            ".incoming-4242",
            "18.pending-4242",
            "18incoming-4242",
            "18.incoming-42.42",
            // `u64::from_str` accepts a leading `+` and leading zeros; the producer writes
            // neither ([`crate::store::parse_build_name`]), so such a directory is not ours.
            "+18.incoming-1",
            "018.incoming-1",
            "+0.incoming-1",
            "0018.superseded-4242",
        ] {
            assert!(!is_stage_scratch(theirs), "{theirs} is NOT ours to delete");
        }
    }

    /// A directory that merely LOOKS scratch-ish is left alone by a real pass — the unit
    /// test above pins the predicate, this pins that `run` actually consults it.
    #[test]
    fn a_lookalike_directory_survives_a_gc_pass() {
        let l = layout("sweep-lookalike");
        seed(&l, "ay", 18, false);
        seed(&l, "ay", 19, true);
        let theirs = seed_scratch(&l, "ay", "notes.incoming-drafts");
        // Build-number lookalikes, which `u64::from_str` alone would have accepted: the
        // first as build 20's scratch (swept by the scratch arm), the second as a
        // marker-less build 21 (swept by the partial arm).
        let plus_scratch = seed_scratch(&l, "ay", "+20.incoming-4242");
        let plus_build = seed_scratch(&l, "ay", "+21");
        let ours = seed_scratch(&l, "ay", "20.incoming-4242");

        let report = run(&l);
        assert!(!ours.exists(), "our own scratch is still swept");
        assert!(theirs.exists(), "a user directory is not ours to delete");
        assert!(
            plus_scratch.exists(),
            "`+20.incoming-4242` is not a name this manager ever wrote"
        );
        assert!(
            plus_build.exists(),
            "`+21` is not a build directory this manager ever wrote"
        );
        assert_eq!(
            report.swept_scratch,
            vec![("ay".to_string(), vec!["20.incoming-4242".to_string()])]
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A marker-bearing lookalike must not displace the rollback target. A bare
    /// `u64::from_str` reads `+19/` with a valid `+19.ready` beside it as "installed build
    /// 19", a number addressed thereafter by its canonical spelling ([`Layout::build_dir`])
    /// — a different directory entirely. The retention rule keeps the live build and the
    /// highest installed build below it ([`reclaimable`]), so that phantom
    /// takes the rollback slot from a real build, which is then reclaimed for it.
    #[test]
    fn a_marker_bearing_lookalike_cannot_displace_the_rollback_target() {
        let l = layout("lookalike-rollback-slot");
        let real_rollback = seed(&l, "ay", 17, false);
        seed(&l, "ay", 25, true); // ay@25 is live
        // `+19`: complete to every marker-reading gate, and a name the producer never writes.
        let lookalike = l.prefix.join("store").join("ay").join("+19");
        std::fs::create_dir_all(lookalike.join("bin")).unwrap();
        std::fs::write(
            lookalike.join("bin").join(tool("ay").exe_file()),
            b"#!/bin/true\n",
        )
        .unwrap();
        crate::store::mark_build_ready(&lookalike).unwrap();
        assert!(
            crate::store::build_is_complete(&lookalike),
            "the fixture must look every bit as complete as a real build, or it proves nothing"
        );

        let report = run(&l);

        assert!(
            real_rollback.exists(),
            "the real rollback target was reclaimed to make room for a build number that \
             never existed: {:?}",
            report.reclaimed
        );
        assert!(
            report.reclaimed.is_empty(),
            "nothing here is reclaimable: {:?}",
            report.reclaimed
        );
        assert!(
            lookalike.exists(),
            "and a directory this manager never wrote is not its to delete"
        );
        assert!(l.build_dir("ay", 25).exists(), "the live build survives");
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// Park a build's tree at `<build>.superseded-<pid>` with nothing at `<build>`: exactly
    /// what a SIGKILL between the swap's two renames leaves. The marker goes with it,
    /// because the swap takes it down before the first rename.
    fn seed_killed_mid_swap(l: &Layout, program: &str, build: u64) -> (PathBuf, PathBuf) {
        let dir = l.build_dir(program, build);
        // Manual concat (no `format!`): Trust-gate lowering workaround — see `lib.rs::dec_u64`.
        let mut name = crate::dec_u64(build);
        name.push_str(".superseded-4242");
        let parked = dir.with_file_name(name);
        crate::store::clear_build_ready(&dir).unwrap();
        std::fs::rename(&dir, &parked).unwrap();
        assert!(
            !dir.exists() && parked.is_dir(),
            "the fixture is only interesting in the two-rename window"
        );
        (dir, parked)
    }

    /// GC run from the other slice must not delete the native install. `build_is_complete`
    /// is per-slice by design, but its `false` means only "not complete for me" — and a
    /// rollback build is claimed by nothing, so a sweep reading that as "no install ever
    /// finished" discards a whole installed toolchain.
    #[test]
    fn a_build_the_other_slice_installed_is_not_an_interrupted_install() {
        let l = layout("other-slice");
        seed(&l, "ay", 18, false); // the rollback build: complete, and claimed by nothing
        seed(&l, "ay", 19, true); // ay@19 is live
        // 18's marker as the other slice wrote it: well formed, simply not our platform.
        let marker = l.prefix.join("store").join("ay").join("18.ready");
        std::fs::write(&marker, b"ok\nplatform=sparc64-solaris\n").unwrap();
        assert!(
            !crate::store::build_is_complete(&l.build_dir("ay", 18)),
            "PRECONDITION: this slice reads it as not-complete and re-stages it"
        );

        let report = run(&l);

        assert!(
            l.build_dir("ay", 18).join("bin").exists(),
            "a finished install of the other slice is not an interrupted one"
        );
        assert!(
            report.swept_partial.is_empty(),
            "and it must not be reported as reclaimed debris: {:?}",
            report.swept_partial
        );

        // Non-vacuity: a tree with no marker at all, claimed by nothing, is still swept.
        let partial = seed_partial(&l, "ay", 17);
        let report = run(&l);
        assert!(
            !partial.exists(),
            "a real interrupted install is still reclaimed"
        );
        assert_eq!(report.swept_partial, vec![("ay".to_string(), vec![17u64])]);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A CRASH IN THE SWAP WINDOW IS RECOVERED, NOT SWEPT. Between `rename(build,
    /// superseded)` and `rename(incoming, build)` the parked tree is the user's ONLY copy.
    /// GC used to `remove_dir_all` it as scratch, so routine housekeeping — not the crash —
    /// was what permanently destroyed the toolchain. It must be moved back instead.
    #[test]
    fn a_tree_parked_by_a_killed_swap_is_moved_back_not_swept() {
        let l = layout("swap-window-recover");
        seed(&l, "ay", 18, false);
        seed(&l, "ay", 19, true);
        let (build19, parked) = seed_killed_mid_swap(&l, "ay", 19);

        let report = run(&l);

        assert!(
            build19.is_dir(),
            "the only copy of the live tree was swept as scratch"
        );
        assert!(
            build19.join("bin").join(tool("ay").exe_file()).exists(),
            "and it is THAT tree, not an empty shell"
        );
        assert!(!parked.exists(), "the scratch name is released");
        assert!(
            report.swept_scratch.is_empty(),
            "a recovered tree must not be reported as reclaimed disk: {:?}",
            report.swept_scratch
        );
        assert!(
            report.swept_partial.is_empty(),
            "and the claim guard must keep the recovered tree: {:?}",
            report.swept_partial
        );
        // Deliberately unmarked: the swap cleared the marker before the rename, so
        // completeness is unrecoverable from disk and the next run re-stages it.
        assert!(
            !crate::store::build_is_complete(&build19),
            "recovery must never claim a completeness it cannot prove"
        );
        assert!(l.build_dir("ay", 18).exists(), "and 18 is untouched");
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// The other half: recovery is not amnesty. A tree parked by a killed swap that NOTHING
    /// claims is an ordinary orphan, and it is swept — as a PARTIAL, under the claim guard,
    /// which is a strictly better question than the scratch arm's (which asks nothing).
    #[test]
    fn a_recovered_tree_nothing_claims_is_still_swept_as_a_partial() {
        let l = layout("swap-window-orphan");
        seed(&l, "ay", 19, true);
        // A second program with no shims and no links at all.
        seed(&l, "ny", 7, false);
        let (build7, parked) = seed_killed_mid_swap(&l, "ny", 7);

        let report = run(&l);

        assert!(!parked.exists(), "the scratch name is released either way");
        assert!(!build7.exists(), "an unclaimed orphan is still reclaimed");
        assert_eq!(
            report.swept_partial,
            vec![("ny".to_string(), vec![7u64])],
            "and it is reported as a partial, not as scratch"
        );
        assert!(report.swept_scratch.is_empty());
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// Recovery is narrow on purpose: two superseded siblings give no way to tell which is
    /// the real outgoing tree, so it refuses to move either — and while nothing stands at
    /// `<build>` both are the only copies of that build on disk, so this sweep must not
    /// delete them either.
    ///
    /// An ambiguous pair with `<build>` absent is therefore reported and left standing: the
    /// program has no live witness, so it lands in `diverged` (the list `atpkg doctor`
    /// prints) and neither tree counts as reclaimed disk. Narrow in the same two directions
    /// the store's guard is — see the `.incoming-<pid>` sibling here, and the test above.
    #[test]
    fn an_ambiguous_pair_with_the_build_absent_is_reported_and_left_standing() {
        let l = layout("swap-window-ambiguous");
        seed(&l, "ay", 19, true);
        let (build19, first) = seed_killed_mid_swap(&l, "ay", 19);
        let second = seed_scratch(&l, "ay", "19.superseded-9999");
        // Non-vacuity: an unverified half-extract is nobody's only copy.
        let incoming = seed_scratch(&l, "ay", "19.incoming-4242");

        let report = run(&l);

        assert!(
            !build19.exists(),
            "an ambiguous window must not be guessed at"
        );
        assert!(
            first.is_dir() && second.is_dir(),
            "the only copies of build 19 went to the sweep the refusal called off"
        );
        assert!(
            first.join("bin").join(tool("ay").exe_file()).exists(),
            "and the parked tree is intact, not an emptied shell"
        );
        assert!(!incoming.exists(), "a half-extract is still swept");
        assert_eq!(
            report.swept_scratch,
            vec![("ay".to_string(), vec!["19.incoming-4242".to_string()])],
            "a tree still standing must not be reported as reclaimed disk"
        );
        assert!(
            report.diverged.iter().any(|d| d.program == "ay"),
            "and the state IS reported, not silently kept: {:?}",
            report.diverged
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// The destructive entry point refuses the witness's own build, and says so, rather than
    /// trusting its caller to have filtered it out.
    #[test]
    fn discard_superseded_refuses_the_live_build() {
        let l = layout("refuse-live");
        seed(&l, "ay", 18, false);
        seed(&l, "ay", 19, true);
        let set = live_builds(&l);
        let witness = set.get("ay").expect("channel + shims agree on 19").clone();
        assert_eq!((witness.program(), witness.build()), ("ay", 19));

        assert_eq!(
            discard_superseded(&l, &witness, 19),
            Err(Retained::IsLive {
                program: "ay".to_string(),
                build: 19,
            })
        );
        assert!(l.build_dir("ay", 19).exists(), "the live tree is untouched");
        // A genuinely superseded build goes.
        assert_eq!(discard_superseded(&l, &witness, 18), Ok(()));
        assert!(!l.build_dir("ay", 18).exists());
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A LANE THAT RESOLVED SOME PROGRAMS SWEEPS ONLY THOSE: the scoped form reclaims the
    /// named program's superseded builds and sweeps its staging and debris, and leaves
    /// every other program's `.part`, carried archive, superseded builds and partial trees
    /// exactly as they were — the whole-prefix form still takes them.
    #[test]
    fn the_scoped_sweep_touches_only_its_programs() {
        let l = layout("scoped");
        for b in [16, 17] {
            seed(&l, "ay", b, false);
        }
        seed(&l, "ay", 18, true);
        for b in [4, 5] {
            seed(&l, "claude", b, false);
        }
        seed(&l, "claude", 6, true);
        let other_partial = seed_partial(&l, "ay", 19);
        let ay_staging = l.staging_dir("ay");
        let claude_staging = l.staging_dir("claude");
        std::fs::create_dir_all(&ay_staging).unwrap();
        std::fs::create_dir_all(&claude_staging).unwrap();
        let resume = ay_staging.join("ay-20.tar.zst.part");
        let carried = ay_staging.join("ay-20.tar.zst");
        let stale = claude_staging.join("claude-5.part");
        let spared = claude_staging.join("claude-7.part");
        for p in [&resume, &carried, &stale, &spared] {
            std::fs::write(p, b"x").unwrap();
        }
        let scope = BTreeSet::from([String::from("claude")]);
        let report = run_for_programs(&l, &scope, &|program| {
            (program == "claude").then(|| String::from("claude-7"))
        });
        assert_eq!(report.reclaimed, vec![(String::from("claude"), vec![4])]);
        assert!(
            resume.exists() && carried.exists(),
            "ay's staging is not ours"
        );
        assert!(l.build_dir("ay", 16).exists(), "ay's builds are not ours");
        assert!(other_partial.exists(), "nor its debris");
        assert!(!stale.exists(), "claude's superseded partial is swept");
        assert!(spared.exists(), "claude's resolved partial is spared");
        assert_eq!(
            report.swept_staging,
            vec![(String::from("claude"), vec![String::from("claude-5.part")])]
        );
        // The whole-prefix form still reclaims what the scoped one left.
        let report = run(&l);
        assert!(!resume.exists() && !l.build_dir("ay", 16).exists());
        assert!(!other_partial.exists());
        assert!(report.reclaimed.contains(&(String::from("ay"), vec![16])));
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// THE resume-across-passes survival rule (R4): gc closes every install/update pass,
    /// so a failed download's `.part` used to die to that same pass's sweep and the next
    /// pass refetched from byte 0. The pass-closing form spares exactly the PINNED
    /// artifact's two files — its `.part` (an interrupted transfer's resume state) and the
    /// complete `<asset>` itself (an aborted coherence group's carried archive, which the
    /// next pass re-stages that member from instead of re-downloading it). Superseded
    /// partials, superseded-asset debris, and other programs' files are swept as before.
    #[test]
    fn the_pass_closing_sweep_spares_the_live_pinned_archive_and_partial_only() {
        let l = layout("staging-keep");
        let staging = l.staging_dir("ay");
        std::fs::create_dir_all(&staging).unwrap();
        let pinned = staging.join("ay-18.tar.zst.part");
        let carried = staging.join("ay-18.tar.zst");
        let superseded = staging.join("ay-17.tar.zst.part");
        let debris = staging.join("ay-17.tar.zst");
        let other = l.staging_dir("ny");
        std::fs::create_dir_all(&other).unwrap();
        let unpinned = other.join("ny-7.tar.zst.part");
        for p in [&pinned, &carried, &superseded, &debris, &unpinned] {
            std::fs::write(p, b"x").unwrap();
        }

        let report = run_keeping_pinned_partials(&l, &|program| {
            (program == "ay").then(|| "ay-18.tar.zst".to_string())
        });

        assert!(
            pinned.exists(),
            "the pinned partial is the next pass's resume state"
        );
        assert!(
            carried.exists(),
            "the pinned COMPLETE archive is an aborted tuple's carry — the next pass \
             re-stages that member from it rather than re-downloading a multi-GB asset"
        );
        assert!(!superseded.exists(), "a superseded partial is still swept");
        assert!(
            !debris.exists(),
            "a SUPERSEDED complete asset is still swept — only the pinned one is spared"
        );
        assert!(
            !unpinned.exists(),
            "a program the caller pins nothing for is fully swept"
        );
        assert_eq!(
            report.swept_staging,
            vec![
                (
                    "ay".to_string(),
                    vec![
                        "ay-17.tar.zst".to_string(),
                        "ay-17.tar.zst.part".to_string()
                    ]
                ),
                ("ny".to_string(), vec!["ny-7.tar.zst.part".to_string()]),
            ],
            "the report lists what went, never what was spared"
        );

        // The standalone verb keeps its full-sweep meaning: the user asked for the disk
        // back, and everything spared above remains reclaimable — a killed download
        // never permanently strands bytes.
        let _ = run(&l);
        assert!(
            !pinned.exists(),
            "a plain `gc` still reclaims the spared partial"
        );
        assert!(
            !carried.exists(),
            "…and the spared archive: an aborted tuple never permanently strands bytes"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// `codex` live at 20 with 18 and 19 below it: under the plain rule 18 goes.
    fn codex_with_a_reclaimable_build(label: &str) -> Layout {
        let l = layout(label);
        seed(&l, "codex", 18, false);
        seed(&l, "codex", 19, false);
        seed(&l, "codex", 20, true);
        l
    }

    /// A LIVE PROCESS KEEPS ITS AGENT BUILD. A session running from a superseded `codex`
    /// build keeps that build through the pass; once nothing runs from it, the ordinary
    /// rule reclaims it.
    #[test]
    fn an_agent_build_a_live_process_runs_from_is_not_reclaimed() {
        let l = codex_with_a_reclaimable_build("agent-live");
        let exe = l.build_dir("codex", 18).join("bin/codex");
        let running = || Some(vec![PathBuf::from("/usr/bin/login"), exe.clone()]);
        let report = run_with(&l, &|_| None, &running);
        assert!(report.reclaimed.is_empty(), "{:?}", report.reclaimed);
        assert!(crate::store::build_is_complete(&l.build_dir("codex", 18)));

        let report = run_with(&l, &|_| None, &|| {
            Some(vec![PathBuf::from("/usr/bin/login")])
        });
        assert_eq!(report.reclaimed, vec![("codex".to_string(), vec![18u64])]);
        assert!(!l.build_dir("codex", 18).exists());
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// FAIL SAFE: a process table that cannot be read keeps every agent build — and
    /// only those: any other program reclaims under the plain rule, so a platform with
    /// no process table does not stop gc for good.
    #[test]
    fn an_unreadable_process_table_keeps_every_agent_build() {
        let l = codex_with_a_reclaimable_build("agent-unknown");
        for b in [16u64, 17] {
            seed(&l, "ay", b, false);
        }
        seed(&l, "ay", 18, true);
        let report = run_with(&l, &|_| None, &|| None);
        assert_eq!(report.reclaimed, vec![("ay".to_string(), vec![16u64])]);
        assert!(l.build_dir("codex", 18).exists());
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// EVERY program is asked about (2026-09-24: a toolchain reclaimed under a running
    /// `targo build` failed it at the next crate), and the process table is read at most
    /// once per pass and only when a build is a candidate.
    #[test]
    fn a_live_process_keeps_any_programs_build_and_the_table_is_read_once() {
        let l = layout("any-once");
        let calls = std::cell::Cell::new(0u32);
        let inside = l.build_dir("ay", 16).join("bin/ay");
        let running = || {
            calls.set(calls.get() + 1);
            Some(vec![inside.clone()])
        };
        seed(&l, "ay", 19, true);
        let _ = run_with(&l, &|_| None, &running);
        assert_eq!(calls.get(), 0, "no candidate, no enumeration");

        for b in [16u64, 17, 18] {
            seed(&l, "ay", b, false);
        }
        seed(&l, "codex", 17, false);
        seed(&l, "codex", 18, false);
        seed(&l, "codex", 19, false);
        seed(&l, "codex", 20, true);
        let report = run_with(&l, &|_| None, &running);
        assert_eq!(
            report.reclaimed,
            vec![
                ("ay".to_string(), vec![17u64]),
                ("codex".to_string(), vec![17u64, 18])
            ],
            "16 runs `ay`: kept"
        );
        assert!(l.build_dir("ay", 16).exists());
        assert_eq!(calls.get(), 1, "one enumeration serves every candidate");
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A TRUST BUILD RUN FROM ITS EXEC ROOT is in use too: a shim routed through
    /// `compat/trust/<build>` runs a clone, not the store's file, and discarding the build
    /// takes that root with it.
    #[test]
    fn a_trust_build_running_from_its_exec_root_is_not_reclaimed() {
        let l = layout("trust-root");
        for b in [8580u64, 8590, 8595] {
            seed(&l, "trust", b, false);
        }
        seed(&l, "trust", 9192, true);
        // 8595 is the rollback target and kept anyway; 8580 and 8590 are candidates.
        let root = crate::compat::root_dir(&l, 8590);
        std::fs::create_dir_all(root.join("bin")).unwrap();
        let exe = root.join("bin/trustc");
        let report = run_with(&l, &|_| None, &|| Some(vec![exe.clone()]));
        assert_eq!(report.reclaimed, vec![("trust".to_string(), vec![8580u64])]);
        assert!(l.build_dir("trust", 8590).exists());
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// An interrupted agent install a process still runs from is not swept as debris.
    #[test]
    fn a_partial_agent_tree_a_process_runs_from_is_not_swept() {
        let l = layout("agent-partial");
        let partial = seed_partial(&l, "claude", 7);
        let exe = partial.join("bin/claude");
        let report = run_with(&l, &|_| None, &|| Some(vec![exe.clone()]));
        assert!(report.swept_partial.is_empty());
        assert!(partial.exists());
        let report = run_with(&l, &|_| None, &|| Some(Vec::new()));
        assert_eq!(
            report.swept_partial,
            vec![("claude".to_string(), vec![7u64])]
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// `runs_from` sees a process by the KERNEL's path, so a launch through a
    /// symlink is still caught — the case `ps`'s argv[0] misses — and a sibling
    /// directory whose name merely starts the same is not.
    #[cfg(target_os = "macos")]
    #[test]
    fn runs_from_sees_a_symlinked_launch_and_not_a_prefix_sibling() {
        let root = layout("runs-from").prefix;
        let app = root.join("A.app");
        let macos = app.join("Contents/MacOS");
        std::fs::create_dir_all(&macos).expect("layout");
        std::fs::copy("/bin/sleep", macos.join("sleepy")).expect("copy sleep");
        let link = root.join("via-link");
        std::os::unix::fs::symlink(macos.join("sleepy"), &link).expect("symlink");
        std::fs::create_dir_all(root.join("A.app.rollback")).expect("sibling");

        assert_eq!(runs_from(&app), Some(false), "nothing runs from it yet");
        let mut child = std::process::Command::new(&link)
            .arg("30")
            .spawn()
            .expect("spawn through the symlink");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while runs_from(&app) != Some(true) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let seen = runs_from(&app);
        let sibling = runs_from(&root.join("A.app.rollback"));
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(seen, Some(true), "a symlinked launch runs from the bundle");
        assert_eq!(sibling, Some(false), "`A.app.rollback` is not `A.app`");
    }

    /// Where no process table is read, the answer is unknown, never "nothing runs".
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    #[test]
    fn without_a_process_table_liveness_is_unknown() {
        assert_eq!(running_executables(), None);
    }

    /// The real enumeration sees this very test process — the proof it reads the table
    /// it claims to, in the resolved spelling [`InUse`] compares against.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn the_process_table_includes_this_process() {
        let me = std::fs::canonicalize(std::env::current_exe().unwrap()).unwrap();
        let running = running_executables().expect("the process table reads");
        assert!(
            running.contains(&me),
            "{} not among {} processes",
            me.display(),
            running.len()
        );
    }

    /// SIDECAR LIFECYCLE. A `.vendor` / `.shim-env` whose build is gone is swept; one
    /// beside a build that exists — complete or partial — is not; and a discard takes the
    /// build's `.vendor` with it.
    #[test]
    fn orphan_sidecars_are_swept_and_a_discard_takes_the_vendor_record() {
        let l = layout("orphan-sidecars");
        seed(&l, "claude", 30, true);
        let beside_live =
            |suffix| crate::store::sidecar_path(&l.build_dir("claude", 30), suffix).unwrap();
        let orphan =
            |b, suffix| crate::store::sidecar_path(&l.build_dir("claude", b), suffix).unwrap();
        for suffix in crate::store::STAGE_SIDECAR_SUFFIXES {
            std::fs::write(beside_live(suffix), b"kept").unwrap();
            std::fs::write(orphan(31, suffix), b"orphan").unwrap();
        }
        let not_ours = l.prefix.join("store/claude/notes.vendor");
        std::fs::write(&not_ours, b"a file the user put here").unwrap();
        let refusal = l.build_dir("claude", 32).with_file_name("32.refused");
        std::fs::write(&refusal, b"stage-refusal v1\n").unwrap();

        let _ = run_with(&l, &|_| None, &|| Some(Vec::new()));
        for suffix in crate::store::STAGE_SIDECAR_SUFFIXES {
            assert!(
                beside_live(suffix).is_file(),
                "{suffix} beside a live build stays"
            );
            assert!(
                !orphan(31, suffix).exists(),
                "{suffix} of a gone build is swept"
            );
        }
        assert!(not_ours.is_file(), "only `<build><suffix>` names are ours");
        assert!(
            refusal.is_file(),
            "a refusal memo outlives its build by design"
        );

        let dir = seed(&l, "claude", 29, false);
        std::fs::write(orphan(29, crate::store::VENDOR_SIDECAR_SUFFIX), b"record").unwrap();
        crate::store::discard_build(&dir);
        assert!(!orphan(29, crate::store::VENDOR_SIDECAR_SUFFIX).exists());
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// The two builds that are ABSENT-LOOKING but not gone keep their sidecars: a partial
    /// tree (kept here because a session runs from it), and a tree a killed swap parked
    /// at `<build>.superseded-<pid>` that recovery could not put back. Once the partial
    /// is let go, its sidecars follow it on the next pass.
    #[test]
    fn sidecars_of_a_partial_or_a_swap_parked_build_are_kept() {
        let l = layout("orphan-sidecars-kept");
        seed(&l, "claude", 40, true);
        let partial = seed_partial(&l, "claude", 34);
        seed(&l, "claude", 33, false);
        let (build33, first) = seed_killed_mid_swap(&l, "claude", 33);
        let second = seed_scratch(&l, "claude", "33.superseded-9999");
        let sidecar =
            |b, suffix| crate::store::sidecar_path(&l.build_dir("claude", b), suffix).unwrap();
        for suffix in crate::store::STAGE_SIDECAR_SUFFIXES {
            std::fs::write(sidecar(34, suffix), b"partial").unwrap();
            std::fs::write(sidecar(33, suffix), b"parked").unwrap();
        }
        let exe = partial.join("bin/claude");

        let _ = run_with(&l, &|_| None, &|| Some(vec![exe.clone()]));
        assert!(
            partial.exists(),
            "the fixture keeps the partial through the pass"
        );
        assert!(
            !build33.exists() && first.is_dir() && second.is_dir(),
            "and the ambiguous swap stays parked"
        );
        for suffix in crate::store::STAGE_SIDECAR_SUFFIXES {
            assert!(sidecar(34, suffix).is_file(), "{suffix} beside a partial");
            assert!(sidecar(33, suffix).is_file(), "{suffix} of a parked build");
        }

        let report = run_with(&l, &|_| None, &|| Some(Vec::new()));
        assert_eq!(
            report.swept_partial,
            vec![("claude".to_string(), vec![34u64])]
        );
        let _ = run_with(&l, &|_| None, &|| Some(Vec::new()));
        for suffix in crate::store::STAGE_SIDECAR_SUFFIXES {
            assert!(
                !sidecar(34, suffix).exists(),
                "{suffix} follows its partial"
            );
            assert!(sidecar(33, suffix).is_file(), "{suffix} of a parked build");
        }
        let _ = std::fs::remove_dir_all(&l.prefix);
    }
}
