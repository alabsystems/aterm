// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! FLIP WHEN QUIET (gap #12(b)): an unattended update stages a new Trust toolchain at once and
//! flips it only when nothing is using the one it replaces.
//!
//! A trust update used to flip within about 30 s of being published, at any hour. The flip
//! re-points `store/trust/current` and the `bin/` shims, and the next re-assertion re-lays
//! the rustup view. A build in flight keeps the compiler it started with (gc keeps the
//! build its processes run from), but a WORKFLOW is several commands: `targo build` then
//! `targo test` through the shim, or a gate's stages, ran on two compilers the moment a
//! flip landed between them — `Dirty …: the toolchain changed`, about 600 packages
//! rebuilt with sccache misses, or E0514 on the `current` path (measured, gap #12). The
//! merge contract and the cutter pin one physical directory per run (2026-09-24); nothing
//! held the flip itself back.
//!
//! # The rule ([`decide`]; the derived model `AtpkgFlipQuiet`)
//!
//! * ONLY THE UNATTENDED LANES WAIT ([`FlipPolicy::WhenQuiet`], carried by
//!   [`crate::cli::DEFER_BUSY_FLIP_FLAG`]): the window's package loop and a terminal
//!   session's detached pass. A person's `aterm pkg update` — typed, or Settings' Update
//!   now — flips at once ([`FlipPolicy::Now`]): they asked for the new toolchain now.
//! * ONLY THE TOOLCHAIN'S GROUP WAITS: the coherence group holding `trust`
//!   ([`crate::seam::SEAM_PROGRAM`]). A singleton verifier flipped under a running
//!   process costs that process nothing — it keeps the build it runs from.
//! * A REVOKED BUILD NEVER WAITS. A yanked, below-floor or tombstoned build — the group's
//!   own, or a member's current one — flips (or is disabled) at once, however busy:
//!   revocation outranks every consumer gate ([`crate::gate`]).
//! * IN USE IS: a run's lease on a member's live build or on the rustup view
//!   ([`crate::lease`]); a process running from a member's build, its exec root or the
//!   view; or NOT KNOWN — the process table or the leases could not be read. Unknown is
//!   priced as in use, the same fail-safe gc and the seam take.
//! * THE WAIT IS BOUNDED: [`CEILING_SECS`] (4 h) from the first deferral, after which the
//!   flip lands anyway and says so. A run pinned to the old build still finishes on it:
//!   gc keeps a leased build whatever flips.
//!
//! # What "staged" means
//!
//! A deferral downloads every member's signed archive into `staging/<program>/` and checks
//! it against its signed digest ([`crate::flow`]'s fetch half), so the flip needs no
//! network — and that archive is exactly what the carried-archive rule
//! (`flow::carried_archive`) re-stages from without a download, re-checking its digest and
//! the extracted tree's signed `tree_root` as every stage does. The tree is extracted when
//! the flip runs, and the flip asks once more, right before it re-points anything: a build
//! that started while the tree was being extracted defers it again (the extracted tree is
//! discarded, the archive kept — the abort path's rule).
//!
//! # The record, and who reads it
//!
//! `<prefix>/flip-deferred.toml` ([`Deferral`]): since when, the builds it moves from and
//! to, the staged archive of each (which every gc spares while the record stands,
//! [`held_archives`]), and why it waited at the last look. `aterm pkg doctor` prints it
//! ([`Deferral::sentence`], naming who looks); the members' status rows say `deferred: …`,
//! which Settings reads. The window's park re-checks it every [`RECHECK`] ([`due`],
//! [`Recheck`]) and wakes a pass when the toolchain is quiet or the ceiling is reached —
//! and with no window open, so does the terminal session watching for updates (the head
//! watch's runner, 2026-09-26). Every pass that moves, holds or settles the group clears it.
//!
//! A flip that lands while a run leases the rustup VIEW (at the ceiling, or a person's
//! update) leaves the view as it stands; `<prefix>/view-left.toml` ([`ViewLeft`]) records
//! it, and the same re-check wakes the pass that lays it once nothing uses it
//! ([`Due::View`]) — not the six-hour walk.

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::store::Layout;

/// How long an unattended flip waits for quiet, at most: four hours from the first
/// deferral. Long enough for a merge-contract run (33 minutes typical) and a release cut
/// to finish on the compiler they started with; short enough that a machine building all
/// day still takes the day's toolchain the same day.
pub const CEILING_SECS: i64 = 4 * 60 * 60;

/// How often the window's park looks at a recorded deferral ([`due`]): a lease read and one
/// process-table walk, a minute apart.
pub const RECHECK: std::time::Duration = std::time::Duration::from_secs(60);

/// `<prefix>/flip-deferred.toml`.
#[must_use]
pub fn record_path(layout: &Layout) -> PathBuf {
    layout.prefix.join("flip-deferred.toml")
}

/// Whether a pass may hold the toolchain's flip for quiet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlipPolicy {
    /// Flip at once: a person asked (`aterm pkg update`, Settings), or a path that never
    /// supersedes a live toolchain (a fresh install).
    Now,
    /// The unattended lanes: stage now, flip when quiet ([`decide`]).
    WhenQuiet,
}

/// What a pass's flip decision reads: the policy, the clock and the process table.
pub struct FlipGate<'a> {
    /// Whether this pass may wait at all.
    pub policy: FlipPolicy,
    /// Unix seconds now.
    pub now: i64,
    /// The process table, as [`crate::gc::running_from`] takes it.
    pub running: &'a dyn Fn() -> Option<Vec<PathBuf>>,
}

/// The table a [`FlipGate::NOW`] never reads.
fn no_table() -> Option<Vec<PathBuf>> {
    None
}

impl FlipGate<'static> {
    /// Flip at once — every path that is not an unattended lane's whole pass.
    pub const NOW: FlipGate<'static> = FlipGate {
        policy: FlipPolicy::Now,
        now: 0,
        running: &no_table,
    };
}

/// What the toolchain a flip would supersede is doing right now ([`probe`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Busy {
    /// Nothing leases it, nothing runs from it.
    Quiet,
    /// A run holds a lease on `what` (a build or the rustup view).
    Leased {
        what: String,
        holders: crate::lease::Holders,
    },
    /// A live process runs from it — the executable the kernel names.
    Running { exe: PathBuf },
    /// It could not be told — priced as in use.
    Unknown { why: String },
}

impl Busy {
    /// Whether the flip may land now.
    #[must_use]
    pub fn is_quiet(&self) -> bool {
        matches!(self, Self::Quiet)
    }

    /// One clause for a person.
    #[must_use]
    pub fn clause(&self) -> String {
        match self {
            Self::Quiet => "nothing is using the toolchain".to_string(),
            Self::Leased { what, holders } => format!("{what} is {}", holders.clause()),
            Self::Running { exe } => format!("{} is running from it", exe.display()),
            Self::Unknown { why } => format!("whether a build is using it cannot be told ({why})"),
        }
    }
}

/// What one look decides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Flip now: a person asked, the build is revoked, or nothing is using the toolchain.
    Flip,
    /// Wait for quiet; the deferral started at `since`.
    Defer { since: i64 },
    /// The wait reached [`CEILING_SECS`] (it started at `since`): flip anyway, and say so.
    FlipAtCeiling { since: i64 },
}

/// THE DECISION, pure: `forced` — a member's pinned or current build is yanked, below the
/// floor or tombstoned — and a [`FlipPolicy::Now`] pass flip at once; so does a quiet
/// toolchain. Otherwise the flip waits, from `since` (the recorded first deferral; `None`,
/// or one after `now` — a clock set back — starts it now) until [`CEILING_SECS`] have
/// passed. The derived model `AtpkgFlipQuiet` states it; the Tier-1 bind below drives this
/// function over every reachable look.
#[must_use]
pub fn decide(
    policy: FlipPolicy,
    forced: bool,
    quiet: bool,
    since: Option<i64>,
    now: i64,
) -> Verdict {
    if policy == FlipPolicy::Now || forced || quiet {
        return Verdict::Flip;
    }
    let since = since.filter(|&s| s <= now).unwrap_or(now);
    if now.saturating_sub(since) >= CEILING_SECS {
        Verdict::FlipAtCeiling { since }
    } else {
        Verdict::Defer { since }
    }
}

/// Whether the toolchain whose live builds are `live` (member → build) is in use: a lease on
/// any of them or on a laid rustup view, else a process running from any of them, a trust
/// build's exec root, or a laid view; the table is read once. Leases first — a few file
/// tests — and the table only when none holds.
#[must_use]
pub fn probe(
    layout: &Layout,
    live: &BTreeMap<String, u64>,
    running: &dyn Fn() -> Option<Vec<PathBuf>>,
) -> Busy {
    let toolchain = live.contains_key(crate::seam::SEAM_PROGRAM);
    let views: Vec<(&str, PathBuf)> = if toolchain {
        crate::seam::SEAM_NAMES
            .iter()
            .map(|name| (*name, crate::seam::view_dir(layout, name)))
            .filter(|(_, dir)| std::fs::symlink_metadata(dir).is_ok())
            .collect()
    } else {
        Vec::new()
    };
    let subjects = live
        .iter()
        .filter_map(|(program, build)| crate::lease::Subject::build(program, *build))
        .chain(
            views
                .iter()
                .filter_map(|(name, _)| crate::lease::Subject::view(name)),
        );
    for subject in subjects {
        let holders = crate::lease::holders(&layout.prefix, &subject);
        if holders.in_use() {
            return Busy::Leased {
                what: subject.describe(),
                holders,
            };
        }
    }
    let Some(table) = running() else {
        return Busy::Unknown {
            why: "the process table could not be read".to_string(),
        };
    };
    let mut dirs: Vec<PathBuf> = Vec::new();
    for (program, build) in live {
        dirs.push(layout.build_dir(program, *build));
        if program == crate::seam::SEAM_PROGRAM {
            dirs.push(crate::compat::root_dir(layout, *build));
        }
    }
    dirs.extend(views.into_iter().map(|(_, dir)| dir));
    let once = || Some(table.clone());
    for dir in dirs {
        if let Some(Some(exe)) = crate::gc::running_from(&dir, &once) {
            return Busy::Running { exe };
        }
    }
    Busy::Quiet
}

/// The recorded deferral of the toolchain's flip.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deferral {
    /// Record schema version.
    pub schema: u32,
    /// The coherence group whose flip waits (`rustc`).
    pub group: String,
    /// Unix seconds of the first deferral of this wait — what the ceiling counts from.
    pub since: i64,
    /// Unix seconds of the last look.
    pub checked: i64,
    /// Why the last look waited ([`Busy::clause`]).
    pub why: String,
    /// Member → the live build the flip moves off.
    #[serde(default)]
    pub from: BTreeMap<String, u64>,
    /// Member → the staged build the flip moves onto.
    #[serde(default)]
    pub to: BTreeMap<String, u64>,
    /// Member → the file name of its staged archive in `staging/<member>/` — what the flip
    /// re-stages from with no download (`flow::carried_archive`), and so what every gc spares
    /// while the wait stands ([`held_archives`]). Absent in a record an older build wrote:
    /// then nothing is spared beyond what the pass itself resolved, as before.
    #[serde(default)]
    pub assets: BTreeMap<String, String>,
}

/// What a held flip moves, as its record keeps it ([`note_deferred`]): the live builds it
/// moves off, the staged ones it moves onto, and the staged archive of each.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Moves {
    /// Member → the live build the flip moves off.
    pub from: BTreeMap<String, u64>,
    /// Member → the staged build the flip moves onto.
    pub to: BTreeMap<String, u64>,
    /// Member → its staged archive's file name in `staging/<member>/`.
    pub assets: BTreeMap<String, String>,
}

/// The one schema this build writes.
const SCHEMA: u32 = 1;

/// The record's size bound: a few hundred bytes in practice.
const MAX_RECORD_BYTES: usize = 64 * 1024;

impl Deferral {
    /// When the wait ends whatever is using the toolchain.
    #[must_use]
    pub fn ceiling_at(&self) -> i64 {
        self.since.saturating_add(CEILING_SECS)
    }

    /// The moves, as a person reads them: `trust build 9192 → 9200, trust-ir build 40 → 41`.
    #[must_use]
    pub fn moves(&self) -> String {
        let mut out: Vec<String> = Vec::new();
        for (program, to) in &self.to {
            match self.from.get(program) {
                Some(from) => out.push(format!("{program} build {from} \u{2192} {to}")),
                None => out.push(format!("{program} build {to}")),
            }
        }
        out.join(", ")
    }

    /// The doctor's line: what waits, since when, why, until when at the latest — and WHO
    /// looks, since "at the latest" is only as true as the host that wakes the pass
    /// ([`Recheck`]): an aterm window's park, or with no window the terminal session watching
    /// for updates (`watcher`, [`crate::vendor_direct::watch::watcher`]). With neither, the
    /// line says the flip waits for the next pass instead of promising a time (2026-09-26).
    /// Then the one command that installs it now.
    #[must_use]
    pub fn sentence(&self, now: i64, watcher: &crate::vendor_direct::watch::Watcher) -> String {
        use crate::vendor_direct::watch::Watcher;
        let looks = match watcher {
            Watcher::Window => Some("an aterm window looks every minute".to_string()),
            Watcher::Session(Some(pid)) => Some(format!(
                "the terminal session watching for updates (pid {pid}) looks every minute"
            )),
            Watcher::Session(None) => {
                Some("the terminal session watching for updates looks every minute".to_string())
            }
            Watcher::Nobody | Watcher::Unknown(_) => None,
        };
        let late = match (now >= self.ceiling_at(), looks, watcher) {
            (true, Some(looks), _) => {
                format!("its wait is over, and {looks}: the next look installs it")
            }
            (false, Some(looks), _) => format!(
                "it installs as soon as nothing is using the toolchain, and by {} at the latest \
                 \u{2014} {looks}",
                clock(self.ceiling_at())
            ),
            (true, None, _) => "its wait is over, so the next update pass installs it".to_string(),
            (false, None, Watcher::Unknown(why)) => format!(
                "it installs as soon as nothing is using the toolchain, and by {} at the latest \
                 while an aterm window or terminal session watches for updates (whether one \
                 does cannot be told: {why})",
                clock(self.ceiling_at())
            ),
            (false, None, _) => format!(
                "no aterm window or terminal session is watching for updates to look at it, so \
                 it installs at the first update pass that finds nothing using the toolchain, or \
                 the first after {} \u{2014} the next aterm window or session starts one",
                clock(self.ceiling_at())
            ),
        };
        format!(
            "the Trust toolchain update ({}) is staged and waiting since {}: {} \u{2014} {late}; \
             `aterm pkg update` installs it now",
            self.moves(),
            clock(self.since),
            self.why
        )
    }
}

/// `2026-09-26 14:05 UTC`, or `an unknown time` for a stamp that is no time.
#[must_use]
pub(crate) fn clock(unix: i64) -> String {
    match u64::try_from(unix) {
        Ok(secs) if unix > 0 && unix != i64::MAX => {
            let stamp = aterm_types::rfc3339::format_rfc3339(secs);
            // `YYYY-MM-DDTHH:MM:SSZ` → `YYYY-MM-DD HH:MM UTC`.
            match (stamp.get(..10), stamp.get(11..16)) {
                (Some(day), Some(time)) => format!("{day} {time} UTC"),
                _ => stamp,
            }
        }
        _ => "an unknown time".to_string(),
    }
}

/// The recorded deferral, if one stands. A record that cannot be read or parsed is none —
/// the next deferral writes a fresh one, and the ceiling then counts from it (a lost record
/// costs at most one more ceiling, never a flip under a build).
#[must_use]
pub fn read(layout: &Layout) -> Option<Deferral> {
    let text =
        crate::metadata_io::read_bounded_regular_utf8(&record_path(layout), MAX_RECORD_BYTES)
            .ok()?;
    aterm_toml::from_str::<Deferral>(&text)
        .ok()
        .filter(|d| d.schema == SCHEMA)
}

/// Write the record: temp + rename, so a reader sees the old record or the new one.
///
/// # Errors
/// The render's or the write's.
pub(crate) fn write(layout: &Layout, record: &Deferral) -> io::Result<()> {
    let text = aterm_toml::to_string(record).map_err(|e| io::Error::other(e.to_string()))?;
    let dest = record_path(layout);
    let tmp = dest.with_file_name(format!("flip-deferred.toml.tmp-{}", std::process::id()));
    let written = (|| {
        use std::io::Write as _;
        let mut f = crate::platform::open_create_write(&tmp, 0o600)?;
        f.write_all(text.as_bytes())?;
        drop(f);
        std::fs::rename(&tmp, &dest)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// Drop the record — the group moved, was held for another reason, or settled.
pub(crate) fn clear(layout: &Layout) {
    let path = record_path(layout);
    if std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
        let _ = std::fs::remove_file(path);
    }
}

/// Record a deferral of `group` at `now`: `since` kept from a standing record of the same
/// group — the ceiling counts from the FIRST deferral of this wait, even when a newer
/// build was published meanwhile — else `since`.
///
/// Whether the record was WRITTEN. A wait is bounded only by the record's `since` — the
/// next pass reads it back to know how long the flip has waited — so a wait that cannot be
/// recorded would start again at every pass and never reach [`CEILING_SECS`]: the caller
/// flips at once instead (review of 2026-09-26).
#[must_use]
pub(crate) fn note_deferred(
    layout: &Layout,
    group: &str,
    since: i64,
    now: i64,
    busy: &Busy,
    moves: &Moves,
) -> bool {
    write(
        layout,
        &Deferral {
            schema: SCHEMA,
            group: group.to_string(),
            since,
            checked: now,
            why: busy.clause(),
            from: moves.from.clone(),
            to: moves.to.clone(),
            assets: moves.assets.clone(),
        },
    )
    .is_ok()
}

/// Whether a standing record still describes a wait: at least one build it moves off is in
/// the store. One that outlived every build it names (an `uninstall --all`, a store emptied
/// by hand) waits for nothing, and nothing is kept or woken for it.
fn stands(layout: &Layout, record: &Deferral) -> bool {
    record.from.iter().any(|(program, build)| {
        std::fs::symlink_metadata(layout.build_dir(program, *build)).is_ok()
    })
}

/// What the deferral record says to a reader that reports it ([`held`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Held {
    /// There is no record.
    Nothing,
    /// The record describes a wait: a build it moves off is still in the store.
    Waits(Deferral),
    /// The record outlived every build it moves off (a store emptied by hand, or by an
    /// older atpkg's uninstall) and waits for nothing — nothing is kept or woken for it.
    /// The next update pass ends it, and so does `aterm pkg repair`.
    Outlived(Deferral),
}

/// The deferral record as `aterm pkg doctor` reports it (2026-09-26): doctor printed "the
/// Trust toolchain update … is staged and waiting" for a record that outlived every build it
/// moves off — after an `uninstall --all` nothing was installed at all. A record [`stands`]
/// refuses is said as what it is. READ-ONLY, as doctor is (review of 1686f3bfa, 2026-09-26:
/// the first cut cleared the record from here, and made the one no-mutation report a store
/// writer): the doors that write end it ([`end_outlived`]).
#[must_use]
pub fn held(layout: &Layout) -> Held {
    match read(layout) {
        None => Held::Nothing,
        Some(record) if stands(layout, &record) => Held::Waits(record),
        Some(record) => Held::Outlived(record),
    }
}

/// End the record when it outlived every build it moves off — what `uninstall` and `repair`
/// run, under the store lock their door holds, so no update pass can be rewriting it
/// meanwhile. `true` when a record was removed; one that still [`stands`] is kept.
pub(crate) fn end_outlived(layout: &Layout) -> bool {
    if !read(layout).is_some_and(|record| !stands(layout, &record)) {
        return false;
    }
    clear(layout);
    read(layout).is_none()
}

/// THE ARCHIVES A HELD FLIP WAITS ON, which every gc spares (2026-09-26): member → the file
/// name of its staged archive in `staging/<member>/`, from the standing record. The update
/// pass that held the flip spared them at its own end, but an `install`, the seed and
/// `atpkg gc` resolve other programs, or none, and swept them — so the quiet flip
/// downloaded the whole toolchain again, and "the flip needs no network" held only until
/// any of them ran. Empty when no record stands, when it outlived the builds it moves off
/// ([`stands`]: `uninstall`, and `uninstall --all`, remove the program's builds before
/// their sweep, so the archive of a flip that can no longer land goes with them), or for a
/// name that is not one bare file name. Bounded by the record: the flip that lands, or the
/// pass that ends the wait, clears it, and the archive is swept like any other.
#[must_use]
pub fn held_archives(layout: &Layout) -> BTreeMap<String, String> {
    let Some(record) = read(layout).filter(|r| stands(layout, r)) else {
        return BTreeMap::new();
    };
    record
        .assets
        .into_iter()
        .filter(|(_, asset)| {
            let mut parts = std::path::Path::new(asset).components();
            matches!(
                (parts.next(), parts.next()),
                (Some(std::path::Component::Normal(_)), None)
            ) && !asset.contains(['/', '\\'])
        })
        .collect()
}

/// The standing record's `since` for `group`, if one stands.
#[must_use]
pub(crate) fn since_of(layout: &Layout, group: &str) -> Option<i64> {
    read(layout).filter(|d| d.group == group).map(|d| d.since)
}

/// Why the window's park — or the terminal session watching for updates — should wake a
/// pass for a held toolchain move ([`due`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Due {
    /// Nothing is using the toolchain now.
    Quiet,
    /// The wait reached its ceiling.
    Ceiling,
    /// A rustup view a flip left behind — leased or run from when the flip landed — is
    /// used by nothing now, and the pass lays it ([`ViewLeft`]).
    View,
}

/// A recorded deferral that is due, and WHICH look of it: the record's `checked` stamp,
/// which every pass that holds the flip again rewrites (for [`Due::View`], the view
/// record's). A caller that already woke a pass for one stamp does not wake another for it
/// — a pass that could not flip (an aborted download, a store it no longer manages) leaves
/// the stamp as it was, and waking again would run a pass a minute for nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DueFlip {
    /// Why it is due.
    pub why: Due,
    /// The record's `checked` stamp at this look.
    pub checked: i64,
}

/// Whether a held toolchain move is due now: a recorded deferral whose toolchain is quiet or
/// whose wait reached its ceiling, else a rustup view a flip left behind that nothing uses
/// any more ([`view_due`]). `None` when neither stands, when the deferral outlived every
/// build it moves off (a record some other path outlived), or when what waits is still in
/// use (or cannot be told) and the ceiling is not reached. What the window's park — or the
/// terminal session watching for updates ([`Recheck`]) — asks every [`RECHECK`]; the pass
/// it wakes decides again for itself. Read-only.
#[must_use]
pub fn due(layout: &Layout, now: i64) -> Option<DueFlip> {
    due_with(layout, now, &crate::gc::process_table)
}

/// [`due`] over an injected process table.
#[must_use]
pub fn due_with(
    layout: &Layout,
    now: i64,
    running: &dyn Fn() -> Option<Vec<PathBuf>>,
) -> Option<DueFlip> {
    flip_due(layout, now, running).or_else(|| view_due(layout, running))
}

/// [`due_with`]'s half for the recorded deferral of the flip itself.
fn flip_due(
    layout: &Layout,
    now: i64,
    running: &dyn Fn() -> Option<Vec<PathBuf>>,
) -> Option<DueFlip> {
    let record = read(layout).filter(|r| stands(layout, r))?;
    let busy = probe(layout, &record.from, running);
    let why = match decide(
        FlipPolicy::WhenQuiet,
        false,
        busy.is_quiet(),
        Some(record.since),
        now,
    ) {
        Verdict::Flip => Due::Quiet,
        Verdict::FlipAtCeiling { .. } => Due::Ceiling,
        Verdict::Defer { .. } => return None,
    };
    Some(DueFlip {
        why,
        checked: record.checked,
    })
}

/// `<prefix>/view-left.toml`: the rustup views a pass left as they stood ([`ViewLeft`]).
#[must_use]
pub fn view_record_path(layout: &Layout) -> PathBuf {
    layout.prefix.join("view-left.toml")
}

/// A RUSTUP VIEW LEFT BEHIND, recorded so it is laid as soon as nothing uses it
/// (2026-09-26). A flip that lands while a run holds a lease on the view — at the ceiling,
/// or a person's `aterm pkg update` — re-points the store at once, but the seam leaves a
/// leased (or run-from) view as it stands ([`crate::seam`]'s `Deferred`). The flip's own
/// record is cleared with the flip, so nothing woke for the view: `cargo +trust` ran the
/// previous compiler until the next pass re-asserted the seam — up to the six-hour walk.
/// The pass that leaves a view writes this; the park's re-check ([`due`], [`Due::View`])
/// wakes a pass once the view is quiet; the pass that lays it — or finds it current —
/// clears it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewLeft {
    /// The build the view was not re-laid onto.
    pub build: String,
    /// Unix seconds the view was first left behind in this stretch.
    pub since: i64,
    /// Unix seconds of the pass that last left it — the stamp a wake is keyed on.
    pub checked: i64,
    /// Why it was left, as the seam said it.
    pub why: String,
}

/// The views recorded left behind, by seam name.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewsLeft {
    /// Record schema version.
    pub schema: u32,
    /// Seam name → what was left.
    #[serde(default)]
    pub views: BTreeMap<String, ViewLeft>,
}

/// The recorded views left behind; none when no record stands or it cannot be read (a lost
/// record costs one wait for the next pass, never a view re-laid under a build).
#[must_use]
pub fn views_left(layout: &Layout) -> ViewsLeft {
    crate::metadata_io::read_bounded_regular_utf8(&view_record_path(layout), MAX_RECORD_BYTES)
        .ok()
        .and_then(|text| aterm_toml::from_str::<ViewsLeft>(&text).ok())
        .filter(|r| r.schema == SCHEMA)
        .unwrap_or_default()
}

/// Write `record` (temp + rename), or remove the file when it holds no view.
fn write_views_left(layout: &Layout, record: &ViewsLeft) -> io::Result<()> {
    let dest = view_record_path(layout);
    if record.views.is_empty() {
        return match std::fs::remove_file(&dest) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    let text = aterm_toml::to_string(record).map_err(|e| io::Error::other(e.to_string()))?;
    let tmp = dest.with_file_name(format!("view-left.toml.tmp-{}", std::process::id()));
    let written = (|| {
        use std::io::Write as _;
        let mut f = crate::platform::open_create_write(&tmp, 0o600)?;
        f.write_all(text.as_bytes())?;
        drop(f);
        std::fs::rename(&tmp, &dest)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// Record that a pass left the view `name` as it stood, not re-laid onto `build`, at `now`:
/// `since` kept from a standing entry, `checked` this pass's. Best-effort: a view whose
/// record cannot be written is laid by the next pass that finds it quiet, as before.
pub(crate) fn note_view_left(
    layout: &Layout,
    name: &str,
    build: &std::path::Path,
    why: &str,
    now: i64,
) {
    let mut record = views_left(layout);
    record.schema = SCHEMA;
    let since = record.views.get(name).map_or(now, |v| v.since.min(now));
    record.views.insert(
        name.to_string(),
        ViewLeft {
            build: build.display().to_string(),
            since,
            checked: now,
            why: why.to_string(),
        },
    );
    let _ = write_views_left(layout, &record);
}

/// Every view left behind ends: rustup is gone, so no seam reaches any view and nothing
/// waits for one to be laid — a rustup that comes back is attached, and its view laid, by the
/// next re-assertion (`seam::reassert`). Best-effort, like every write of this record.
pub(crate) fn clear_views_left(layout: &Layout) {
    let _ = write_views_left(layout, &ViewsLeft::default());
}

/// The view `name` is as it should be — laid, current, or gone with its seam: its entry ends.
pub(crate) fn clear_view_left(layout: &Layout, name: &str) {
    let mut record = views_left(layout);
    if record.views.remove(name).is_some() {
        let _ = write_views_left(layout, &record);
    }
}

/// [`due_with`]'s half for the views a flip left behind: the first recorded view that is
/// laid and that nothing uses — no lease on it ([`crate::lease`]) and no process running
/// from it — is due, keyed by its entry's `checked` stamp. A view whose leases or process
/// table cannot be read is in use, as everywhere else.
fn view_due(layout: &Layout, running: &dyn Fn() -> Option<Vec<PathBuf>>) -> Option<DueFlip> {
    let record = views_left(layout);
    for (name, left) in &record.views {
        let view = crate::seam::view_dir(layout, name);
        if std::fs::symlink_metadata(&view).is_err() {
            continue;
        }
        if crate::lease::Subject::view(name)
            .is_some_and(|s| crate::lease::holders(&layout.prefix, &s).in_use())
        {
            continue;
        }
        if crate::gc::running_from(&view, running) == Some(None) {
            return Some(DueFlip {
                why: Due::View,
                checked: left.checked,
            });
        }
    }
    None
}

/// THE RE-CHECK OF A HELD TOOLCHAIN MOVE, as every host runs it — the window's package
/// park and the terminal session watching for updates ([`crate::vendor_direct::watch`]'s
/// runner): [`due`] asked at most every [`RECHECK`], never every slice (with nothing
/// standing, a look is one failed file read). Quiet counts only on TWO looks in a row, a
/// minute apart: the moment between a `targo build` and the `targo test` after it is quiet
/// for a second, and a flip — or a view re-laid — there splits the two across compilers,
/// the rebuild the wait exists to avoid. The two looks are of ONE record look — the same
/// move (the flip, or a view) at the same stamp: [`due`] answers for the flip before the
/// view, so a view quiet while a build runs from the store's toolchain must not count as
/// the flip's first look, and a stamp a pass wrote again says that pass found the move in
/// use (review, 2026-09-26). The ceiling wakes at once. ONE WAKE PER LOOK OF A RECORD: a
/// pass that held the move again rewrote its stamp; one that could not make it (an aborted
/// download) left it, and waking again would run a pass a minute for nothing.
#[derive(Clone, Debug, Default)]
pub struct Recheck {
    /// When [`Self::wake`] last looked.
    last: Option<std::time::Instant>,
    /// The record look that last look found quiet — the first of the two a wake needs:
    /// whether it was a view's, and its stamp.
    quiet_seen: Option<(bool, i64)>,
    /// The record look the last wake was for: whether it was a view's, and its stamp.
    woke_for: Option<(bool, i64)>,
}

impl Recheck {
    /// Whether a look is due, without reading the store or taking a watch claim. A
    /// session uses this to skip the rendezvous between its completed looks; a
    /// refused claim is retried on the next park slice without advancing `last`.
    #[must_use]
    pub fn look_due(&self, now: std::time::Instant) -> bool {
        self.last
            .is_none_or(|at| now.saturating_duration_since(at) >= RECHECK)
    }

    /// Whether to wake a pass now, `due` asked only when a look is due at `now` (a
    /// monotonic instant).
    pub fn wake(&mut self, now: std::time::Instant, due: impl FnOnce() -> Option<DueFlip>) -> bool {
        if !self.look_due(now) {
            return false;
        }
        self.last = Some(now);
        let Some(flip) = due() else {
            self.quiet_seen = None;
            return false;
        };
        let look = (flip.why == Due::View, flip.checked);
        if self.woke_for == Some(look) {
            self.quiet_seen = None;
            return false;
        }
        let wake = match flip.why {
            Due::Ceiling => true,
            Due::Quiet | Due::View => self.quiet_seen == Some(look),
        };
        self.quiet_seen = (!wake && flip.why != Due::Ceiling).then_some(look);
        if wake {
            self.woke_for = Some(look);
        }
        wake
    }
}

/// Whether the held move that last woke this host still stands at the same record look.
/// A scheduled pass can wait behind another host's pass after its wake; that pass may
/// clear the move or re-record it with a new `checked` stamp. The old wake then must
/// not run a second whole update. A new stamp gets its own two quiet looks (or ceiling
/// wake) through [`Recheck`]. This is a small local record read, not a process-table probe.
#[must_use]
pub fn woken_move_still_stands(layout: &Layout, recheck: &Recheck) -> bool {
    match recheck.woke_for {
        Some((false, checked)) => {
            matches!(held(layout), Held::Waits(record) if record.checked == checked)
        }
        Some((true, checked)) => views_left(layout)
            .views
            .values()
            .any(|record| record.checked == checked),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(label: &str) -> Layout {
        let p = std::env::temp_dir().join(format!("atpkg-quiet-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Layout { prefix: p }
    }

    const T0: i64 = 1_790_000_000;

    #[test]
    fn a_person_a_revocation_or_quiet_flips_at_once() {
        for (policy, forced, quiet) in [
            (FlipPolicy::Now, false, false),
            (FlipPolicy::WhenQuiet, true, false),
            (FlipPolicy::WhenQuiet, false, true),
        ] {
            assert_eq!(
                decide(policy, forced, quiet, Some(T0 - 60), T0),
                Verdict::Flip,
                "{policy:?} forced={forced} quiet={quiet}"
            );
        }
    }

    #[test]
    fn a_busy_toolchain_waits_up_to_the_ceiling_counted_from_the_first_deferral() {
        let wait = |since, now| decide(FlipPolicy::WhenQuiet, false, false, since, now);
        assert_eq!(wait(None, T0), Verdict::Defer { since: T0 });
        assert_eq!(
            wait(Some(T0), T0 + CEILING_SECS - 1),
            Verdict::Defer { since: T0 }
        );
        assert_eq!(
            wait(Some(T0), T0 + CEILING_SECS),
            Verdict::FlipAtCeiling { since: T0 }
        );
        // A clock set back behind the record restarts the wait from now — bounded again —
        // rather than waiting until the clock catches up with a future stamp.
        assert_eq!(wait(Some(T0 + 3600), T0), Verdict::Defer { since: T0 });
    }

    #[test]
    fn the_record_round_trips_and_keeps_its_first_since() {
        let l = layout("record");
        assert_eq!(read(&l), None);
        let from = BTreeMap::from([("trust".to_string(), 9192u64)]);
        let to = BTreeMap::from([("trust".to_string(), 9200u64)]);
        let busy = Busy::Running {
            exe: PathBuf::from("/x/targo"),
        };
        assert!(note_deferred(
            &l,
            "rustc",
            T0,
            T0,
            &busy,
            &Moves {
                from: from.clone(),
                to: to.clone(),
                assets: BTreeMap::new(),
            },
        ));
        let d = read(&l).unwrap();
        assert_eq!((d.since, d.checked, d.group.as_str()), (T0, T0, "rustc"));
        assert_eq!(d.moves(), "trust build 9192 \u{2192} 9200");
        assert_eq!(since_of(&l, "rustc"), Some(T0));
        assert_eq!(since_of(&l, "other"), None);
        use crate::vendor_direct::watch::Watcher;
        let line = d.sentence(T0 + 60, &Watcher::Window);
        for part in [
            "trust build 9192 \u{2192} 9200",
            "/x/targo is running from it",
            "by ",
            "an aterm window looks every minute",
            "`aterm pkg update` installs it now",
        ] {
            assert!(line.contains(part), "{part}: {line}");
        }
        // WHO LOOKS decides what "at the latest" may promise.
        assert!(
            d.sentence(T0 + 60, &Watcher::Session(Some(42)))
                .contains("the terminal session watching for updates (pid 42) looks every minute")
        );
        let unwatched = d.sentence(T0 + 60, &Watcher::Nobody);
        assert!(
            unwatched.contains("no aterm window or terminal session is watching")
                && !unwatched.contains("at the latest"),
            "{unwatched}"
        );
        assert!(
            d.sentence(T0 + 60, &Watcher::Unknown("x".into()))
                .contains("while an aterm window or terminal session watches for updates")
        );
        assert!(
            d.sentence(d.ceiling_at(), &Watcher::Window)
                .contains("its wait is over, and an aterm window looks every minute")
        );
        assert!(
            d.sentence(d.ceiling_at(), &Watcher::Nobody)
                .contains("its wait is over, so the next update pass installs it")
        );
        clear(&l);
        assert_eq!(read(&l), None);
        // A record that cannot be parsed is none, never a flip held forever.
        std::fs::write(record_path(&l), b"schema = \"x\"\n").unwrap();
        assert_eq!(read(&l), None);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// Lay a complete trust build and return its `bin/targo`.
    fn seed_build(l: &Layout, program: &str, build: u64) -> PathBuf {
        let bin = l.build_dir(program, build).join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let exe = bin.join("targo");
        std::fs::write(&exe, b"#!/bin/true\n").unwrap();
        exe
    }

    /// The probe, every arm: a lease on a member's live build, a lease on the laid view, a
    /// process in the build, its exec root or the view, an unreadable table — and quiet.
    #[test]
    fn the_probe_reads_leases_then_the_process_table() {
        let l = layout("probe");
        let exe = seed_build(&l, "trust", 9192);
        seed_build(&l, "trust-ir", 40);
        let live = BTreeMap::from([("trust".to_string(), 9192u64), ("trust-ir".to_string(), 40)]);
        let empty = || Some(Vec::new());
        assert_eq!(probe(&l, &live, &empty), Busy::Quiet);
        assert!(matches!(probe(&l, &live, &|| None), Busy::Unknown { .. }));
        assert_eq!(
            probe(&l, &live, &|| Some(vec![exe.clone()])),
            Busy::Running { exe: exe.clone() }
        );
        let root_exe = crate::compat::root_dir(&l, 9192).join("bin/trustc");
        std::fs::create_dir_all(root_exe.parent().unwrap()).unwrap();
        assert!(matches!(
            probe(&l, &live, &|| Some(vec![root_exe.clone()])),
            Busy::Running { .. }
        ));
        // A process in ANOTHER build of trust is not this toolchain in use.
        let other = seed_build(&l, "trust", 9100);
        assert_eq!(probe(&l, &live, &|| Some(vec![other.clone()])), Busy::Quiet);
        // A member's lease, read before (and without) the table.
        let lease = crate::lease::take(
            &l.prefix,
            &crate::lease::Subject::build("trust-ir", 40).unwrap(),
            "aterm-release cut",
            crate::lease::DEFAULT_WAIT,
        )
        .unwrap();
        let busy = probe(&l, &live, &|| -> Option<Vec<PathBuf>> {
            panic!("a lease answers before the table is read")
        });
        assert_eq!(
            busy.clause(),
            "trust-ir build 40 is in use by aterm-release cut"
        );
        drop(lease);
        // The view: only once it is laid, then by lease and by process.
        let view = crate::seam::view_dir(&l, "trust");
        std::fs::create_dir_all(view.join("bin")).unwrap();
        let in_view = view.join("bin/targo");
        assert!(matches!(
            probe(&l, &live, &|| Some(vec![in_view.clone()])),
            Busy::Running { .. }
        ));
        let lease = crate::lease::take(
            &l.prefix,
            &crate::lease::Subject::view("trust").unwrap(),
            "aterm-verify (pid 7)",
            crate::lease::DEFAULT_WAIT,
        )
        .unwrap();
        assert!(matches!(probe(&l, &live, &empty), Busy::Leased { .. }));
        drop(lease);
        assert_eq!(probe(&l, &live, &empty), Busy::Quiet);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// TIER-1: the REAL decision ([`decide`]) and the REAL in-use classification
    /// ([`Busy::is_quiet`]) against the derived model `AtpkgFlipQuiet`, at every reachable
    /// look — the model's `busy` mapped onto a [`Busy`] (0 quiet, 1 a process running from
    /// it, 2 a table that cannot be read), its `forced` onto the revocation input, and its
    /// `age` onto the clock in periods of `CEILING_SECS / Ceiling`, so the model's ceiling
    /// is the real one. Exactly one of `Flip`/`Hold` is enabled at every look, and the real
    /// verdict matches it. Negative control: the decision with an unreadable table taken
    /// for quiet disagrees at a reachable look, so agreement is never vacuous.
    #[test]
    fn the_flip_decision_refines_the_derived_model() {
        use aterm_spec::derive::atpkg_flip_quiet_model;
        let model = atpkg_flip_quiet_model();
        let ceiling = model
            .consts
            .iter()
            .find(|(name, _)| *name == "Ceiling")
            .map(|(_, v)| *v)
            .expect("the model names its ceiling");
        let period = CEILING_SECS / ceiling;
        assert_eq!(
            period * ceiling,
            CEILING_SECS,
            "the ceiling maps onto whole periods"
        );
        let mut seen = std::collections::BTreeSet::new();
        let mut frontier = vec![model.init_state()];
        while let Some(state) = frontier.pop() {
            if !seen.insert(state.clone()) {
                continue;
            }
            for action in &model.actions {
                let mut next = state.clone();
                if model.fire(action.name, &mut next) {
                    frontier.push(next);
                }
            }
        }
        let looks: Vec<_> = seen
            .iter()
            .filter(|s| s["pending"] == 1 && s["looked"] == 0)
            .collect();
        assert!(
            looks.len() >= 16,
            "the explorer reached the looks: {looks:?}"
        );
        let busy_of = |b: i64| match b {
            0 => Busy::Quiet,
            1 => Busy::Running {
                exe: PathBuf::from("/x/targo"),
            },
            _ => Busy::Unknown {
                why: "the process table could not be read".to_string(),
            },
        };
        let modeled = |s: &std::collections::BTreeMap<&'static str, i64>| match ["Flip", "Hold"]
            .into_iter()
            .filter(|a| model.action_enabled(a, s))
            .collect::<Vec<_>>()[..]
        {
            ["Flip"] => true,
            ["Hold"] => false,
            ref other => panic!("exactly one of Flip/Hold is enabled at {s:?}: {other:?}"),
        };
        let flips = |v: Verdict| !matches!(v, Verdict::Defer { .. });
        let at = |s: &std::collections::BTreeMap<&'static str, i64>| T0 + s["age"] * period;
        for s in &looks {
            let real = decide(
                FlipPolicy::WhenQuiet,
                s["forced"] == 1,
                busy_of(s["busy"]).is_quiet(),
                Some(T0),
                at(s),
            );
            assert_eq!(
                flips(real),
                modeled(s),
                "the real decision at {s:?}: {real:?}"
            );
            if let Verdict::Defer { since } = real {
                assert_eq!(since, T0, "the wait counts from its first deferral");
            }
        }
        // A lease is in use exactly as a running process is.
        assert!(
            !Busy::Leased {
                what: "trust build 1".into(),
                holders: crate::lease::Holders::Held(vec!["x".into()]),
            }
            .is_quiet()
        );
        // NEGATIVE CONTROL: an unreadable table taken for quiet.
        assert!(
            looks.iter().any(|s| {
                let mutant = decide(
                    FlipPolicy::WhenQuiet,
                    s["forced"] == 1,
                    s["busy"] != 1,
                    Some(T0),
                    at(s),
                );
                flips(mutant) != modeled(s)
            }),
            "the comparison must be able to fail"
        );
    }

    /// The window's re-check: nothing recorded is nothing due; a recorded deferral is due
    /// when the toolchain is quiet, or at the ceiling however busy — named by the record's
    /// `checked` stamp — and a record whose builds are all gone from the store is none.
    #[test]
    fn a_recorded_deferral_is_due_when_quiet_or_at_the_ceiling() {
        let l = layout("due");
        let exe = seed_build(&l, "trust", 9192);
        let busy = || Some(vec![exe.clone()]);
        assert_eq!(due_with(&l, T0, &busy), None, "nothing recorded");
        let from = BTreeMap::from([("trust".to_string(), 9192u64)]);
        let to = BTreeMap::from([("trust".to_string(), 9200u64)]);
        assert!(note_deferred(
            &l,
            "rustc",
            T0,
            T0 + 5,
            &Busy::Quiet,
            &Moves {
                from,
                to,
                assets: BTreeMap::new(),
            },
        ));
        assert_eq!(due_with(&l, T0 + 60, &busy), None);
        assert_eq!(due_with(&l, T0 + 60, &|| None), None, "unknown is busy");
        assert_eq!(
            due_with(&l, T0 + 60, &|| Some(Vec::new())),
            Some(DueFlip {
                why: Due::Quiet,
                checked: T0 + 5
            })
        );
        assert_eq!(
            due_with(&l, T0 + CEILING_SECS, &busy).map(|d| d.why),
            Some(Due::Ceiling)
        );
        std::fs::remove_dir_all(l.build_dir("trust", 9192)).unwrap();
        assert_eq!(
            due_with(&l, T0 + 60, &|| Some(Vec::new())),
            None,
            "a record that outlived the builds it names wakes nothing"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// THE RE-CHECK EVERY HOST RUNS ([`Recheck`]): looked at once a minute, never every
    /// slice; quiet wakes a pass only on two looks in a row — a busy look between them
    /// starts the count over — the ceiling at once, and one wake per look of a record. A
    /// view's look is keyed apart from the flip's, so a stamp the two records share in one
    /// second cannot swallow the view's wake.
    #[test]
    fn the_recheck_wakes_on_two_quiet_looks_or_the_ceiling_once_per_look() {
        let look = |why, checked| move || Some(DueFlip { why, checked });
        let mut r = Recheck::default();
        let t0 = std::time::Instant::now();
        assert!(!r.wake(t0, look(Due::Quiet, 1)), "one quiet look");
        assert!(
            !r.wake(t0 + std::time::Duration::from_secs(5), || {
                panic!("no second look inside the minute")
            }),
            "not looked at again inside the minute"
        );
        assert!(
            r.wake(t0 + RECHECK, look(Due::Quiet, 1)),
            "the second quiet look in a row wakes the pass"
        );
        for n in 2..5 {
            assert!(!r.wake(t0 + n * RECHECK, look(Due::Quiet, 1)), "look {n}");
        }
        assert!(
            !r.wake(t0 + 5 * RECHECK, look(Due::Ceiling, 1)),
            "one wake per look of a record, even at the ceiling"
        );
        let t1 = t0 + 6 * RECHECK;
        assert!(!r.wake(t1, look(Due::Quiet, 2)));
        assert!(!r.wake(t1 + RECHECK, || None));
        assert!(
            !r.wake(t1 + 2 * RECHECK, look(Due::Quiet, 2)),
            "a busy look between two quiet ones starts the count over"
        );
        assert!(r.wake(t1 + 3 * RECHECK, look(Due::Quiet, 2)));
        // The view left behind by the pass that woke carries the same stamp: its own look.
        assert!(!r.wake(t1 + 4 * RECHECK, look(Due::View, 2)));
        assert!(
            r.wake(t1 + 5 * RECHECK, look(Due::View, 2)),
            "a view's look is its own"
        );
        let mut fresh = Recheck::default();
        assert!(
            fresh.wake(t0, look(Due::Ceiling, 3)),
            "the ceiling wakes at once"
        );
    }

    /// A QUIET LOOK COUNTS ONLY FOR THE MOVE IT LOOKED AT (review, 2026-09-26). The two
    /// records share one re-check, and [`due`] answers for the flip first: a view left
    /// behind that was quiet while a build ran from the store's toolchain (the flip busy)
    /// is not the first of the flip's two quiet looks — the gap between that build and the
    /// `targo test` after it would flip on ONE — and a quiet look at the flip is not the
    /// first of a view's. Nor is a look at a record a pass has since written again: that
    /// pass looked, and found the move in use.
    #[test]
    fn a_quiet_look_at_one_move_is_not_the_first_of_two_at_another() {
        let look = |why, checked| move || Some(DueFlip { why, checked });
        let t0 = std::time::Instant::now();
        // The view quiet while the flip is busy, then the flip quiet: one look at the flip.
        let mut r = Recheck::default();
        assert!(!r.wake(t0, look(Due::View, 1)));
        assert!(
            !r.wake(t0 + RECHECK, look(Due::Quiet, 2)),
            "the flip's first quiet look"
        );
        assert!(
            r.wake(t0 + 2 * RECHECK, look(Due::Quiet, 2)),
            "its second wakes the pass"
        );
        // The flip quiet, then (the flip busy again) the view quiet: one look at the view.
        let mut r = Recheck::default();
        assert!(!r.wake(t0, look(Due::Quiet, 1)));
        assert!(
            !r.wake(t0 + RECHECK, look(Due::View, 2)),
            "the view's first quiet look"
        );
        assert!(r.wake(t0 + 2 * RECHECK, look(Due::View, 2)));
        // A pass held the flip again between two quiet looks: the count starts over.
        let mut r = Recheck::default();
        assert!(!r.wake(t0, look(Due::Quiet, 1)));
        assert!(
            !r.wake(t0 + RECHECK, look(Due::Quiet, 5)),
            "the record was written again"
        );
        assert!(r.wake(t0 + 2 * RECHECK, look(Due::Quiet, 5)));
    }

    /// The views-left record round-trips, keeps its first `since`, and goes when its last
    /// view is cleared.
    #[test]
    fn the_views_left_record_round_trips_and_goes_with_its_last_view() {
        let l = layout("views-left");
        assert!(views_left(&l).views.is_empty());
        note_view_left(
            &l,
            "trust",
            std::path::Path::new("/p/store/trust/9200"),
            "it is leased",
            T0,
        );
        note_view_left(
            &l,
            "trust",
            std::path::Path::new("/p/store/trust/9201"),
            "it is leased",
            T0 + 60,
        );
        let v = views_left(&l);
        let trust = &v.views["trust"];
        assert_eq!(
            (trust.build.as_str(), trust.since, trust.checked),
            ("/p/store/trust/9201", T0, T0 + 60)
        );
        clear_view_left(&l, "trust");
        assert!(views_left(&l).views.is_empty());
        assert!(!view_record_path(&l).exists(), "no view, no file");
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn a_woken_move_is_retired_when_another_pass_clears_or_renews_its_record() {
        let l = layout("woken-move-renewal");
        std::fs::create_dir_all(l.build_dir("trust", 1)).unwrap();
        let record = Deferral {
            schema: SCHEMA,
            group: "rustc".into(),
            checked: T0,
            from: [("trust".into(), 1)].into(),
            ..Deferral::default()
        };
        write(&l, &record).unwrap();
        let mut flip = Recheck::default();
        assert!(flip.wake(std::time::Instant::now(), || Some(DueFlip {
            why: Due::Ceiling,
            checked: T0,
        })));
        assert!(woken_move_still_stands(&l, &flip), "unchanged flip");
        write(
            &l,
            &Deferral {
                checked: T0 + 1,
                ..record.clone()
            },
        )
        .unwrap();
        assert!(!woken_move_still_stands(&l, &flip), "renewed flip");
        clear(&l);
        assert!(!woken_move_still_stands(&l, &flip), "completed flip");

        note_view_left(&l, "trust", std::path::Path::new("/tmp/trust"), "busy", T0);
        let mut view = Recheck::default();
        let t0 = std::time::Instant::now();
        assert!(!view.wake(t0, || Some(DueFlip {
            why: Due::View,
            checked: T0,
        })));
        assert!(view.wake(t0 + RECHECK, || Some(DueFlip {
            why: Due::View,
            checked: T0,
        })));
        assert!(woken_move_still_stands(&l, &view), "unchanged view");
        note_view_left(
            &l,
            "trust",
            std::path::Path::new("/tmp/trust"),
            "busy",
            T0 + 1,
        );
        assert!(!woken_move_still_stands(&l, &view), "renewed view");
        clear_view_left(&l, "trust");
        assert!(!woken_move_still_stands(&l, &view), "completed view");
        let _ = std::fs::remove_dir_all(&l.prefix);
    }
}
