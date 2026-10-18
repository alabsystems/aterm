// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The operator-readable status record (`<prefix>/status.toml`): the resolved index
//! source, the last aggregate outcome, the pass stamps and a per-program state line, so an
//! operator can see whether updates arrive and why the last apply did not happen. Status
//! is diagnostics, never load-bearing, and durable:
//!
//! * written through for what a pass must not lose, coalesced for what it stamps: while a
//!   verb holds the store lock ([`hold`]) a change to a row, the seams or any other field
//!   is on disk at once, while the outcome sentence and the clocks land once, at its end;
//! * written by temp + fsync + rename + a directory sync, so a power loss leaves the old
//!   record or the new one;
//! * keys this build does not know ride [`Status::extra`] through every rewrite;
//! * a corrupt record (unparsable, not UTF-8, over its bounds, not a regular file) is kept
//!   as `status.toml.corrupt-<unix>` and replaced in one rename by one rebuilt from the
//!   store ([`seed_for_rewrite`]); one unreadable for another reason is left alone.

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::Layout;

/// Maximum serialized size of the operator-readable status record.
///
/// A normal record is a few KiB. Two MiB leaves room for a large managed fleet
/// while making Settings/doctor/verify parsing and allocation strictly finite.
pub const MAX_STATUS_BYTES: usize = 2 * 1024 * 1024;

/// Maximum number of per-program rows admitted from one status snapshot.
pub const MAX_STATUS_PROGRAMS: usize = 2048;

/// The clause an update pass that did not reach the signed index appends to its `outcome`
/// (`<outcome> (index from cache — <why>)`), naming the cause (a rate limit, a 5xx, a
/// captive portal). One spelling for the writer (`cli`'s `record_index_freshness`) and the
/// window's pass report, which reads it to say the pass ran on a stale index.
pub const INDEX_FROM_CACHE_CLAUSE: &str = "(index from cache \u{2014} ";

/// One program's last-known state, for `status.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProgramStatus {
    /// The currently-active build, if any.
    pub installed_build: Option<u64>,
    /// The program's state line. For a program the index names, one of the canonical
    /// spellings of [`crate::state`] (`managed <build> — pinned by index <N>`, `system:
    /// <path> — not managed by aterm`, `unavailable on <target>: <hint>`, …), the words the
    /// pass log, `doctor` and `which` print. Faults keep prefixed free text (`"error: …"`,
    /// `"tombstoned: yanked@N"`, `"deferred: …"`, `"rejected: …"`), which `doctor` matches
    /// by prefix.
    pub state: String,
    /// The signed `tree_root` (§8) of the active build, from the release-key-verified
    /// manifest at install/update time. `atpkg verify` compares the store tree's recomputed
    /// root to this value, never to a self-generated hash. Empty: no signed root recorded,
    /// so verify reports "cannot attest" (fail-closed).
    #[serde(default)]
    pub tree_root: String,
}

/// The aggregate status snapshot written after a check/apply pass.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Status {
    /// Record schema version.
    pub schema: u32,
    /// RFC3339 UTC time this record was written (the caller stamps it).
    pub updated_at: String,
    /// Whether the manager is configured to act (root key pinned + not opted out).
    pub enabled: bool,
    /// The resolved index source, `owner/repo`.
    pub index_source: String,
    /// The last aggregate decision (`"up to date"`, `"staged …"`, `"idle: no token"`,
    /// `"rejected unsigned index at build N"`, …).
    pub outcome: String,
    /// The rustup toolchain seams atpkg owns, as `rustup:<name>` ([`crate::seam`]): written
    /// when an attach succeeds, dropped by a detach and `uninstall --all`, walked by every
    /// re-assertion; empty in older records. Two more spellings ride beside them, never
    /// read as seams: `refused:rustup:<name>: <why>` (the last re-assertion's refusal) and
    /// `replaced:rustup:<name>: <target>` (the stale link one replaced, the way back:
    /// [`crate::seam::replaced_from`]), spelled `replacing:…` from just before the swap
    /// until the entry names the view.
    #[serde(default)]
    pub seams: Vec<String>,
    /// RFC3339 UTC time of the last full update pass that reached the signed index (one the
    /// channel served this pass, not the §14 cache's) and ran to its end. Stamped only by
    /// [`stamp_success`], at the pass's end (the derived model `AtpkgPassStamps` states the
    /// contract). Empty: no pass has reached the index here, which `aterm pkg doctor`
    /// reports.
    #[serde(default)]
    pub last_success_at: String,
    /// The `index_build` the last pass resolved, and when it last changed: the publisher's
    /// own pulse, which `doctor`'s "publishing looks frozen" reads.
    #[serde(default)]
    pub last_index_build: u64,
    /// See [`Self::last_index_build`].
    #[serde(default)]
    pub index_build_changed_at: String,
    /// How the last full `update` pass ended
    /// ([`aterm_update_core::pkg_check::PassOutcome`]): `ok` (it reached the signed index,
    /// or had nothing to check), `offline` or `failed` (including a pass the §14 cache
    /// served after a refusal). Stamped only at the pass's end; the schedulers read this,
    /// not `updated_at`, which moves on every write.
    #[serde(default)]
    pub last_pass: String,
    /// RFC3339 UTC time that pass ended. See [`Self::last_pass`].
    #[serde(default)]
    pub last_pass_at: String,
    /// The highest index this full pass was asked to check or resolved, when it attempted a
    /// signed index resolve; zero is unknown. Written with `last_pass` and `last_pass_at`, so
    /// a scheduler can tell a newer publication from another lane's attempt at the same
    /// index, even one that failed before resolving the listing.
    #[serde(default)]
    pub last_pass_attempted_index_build: u64,
    /// The exact pass-end stamp that owns `last_pass_attempted_index_build`.
    /// A previous atpkg binary preserves unknown fields while rewriting the
    /// known pass end; comparing this stamp avoids borrowing its old target.
    #[serde(default)]
    pub last_pass_attempted_at: String,
    /// How many full passes have recorded their end on this store, bumped only by the
    /// pass-end stamp, so a queued pass can tell exactly whether one ended since it was
    /// queued (`last_pass_at` is whole seconds). Zero in older records; an older binary
    /// carries it through unmoved.
    #[serde(default)]
    pub pass_seq: u64,
    /// The [`Self::pass_seq`] of the last full pass that ended `failed` for one reason
    /// only: its index came from the §14 cache (the listing was refused, yet some host
    /// answered, so not offline) while no member failed. Such a pass exits 1 with nothing on
    /// stderr; its cause is only [`INDEX_FROM_CACHE_CLAUSE`] in [`Self::outcome`]. Any value
    /// but the current `pass_seq` means the last pass was not that (an older binary that
    /// bumps `pass_seq` without knowing this key leaves it stale, which reads as "not that",
    /// the safe side).
    ///
    /// A `--wait-lock` child that stands down behind such a pass exits as that pass would
    /// have ([`crate::cli`]'s stand-down), and the outcome text alone cannot say whether the
    /// holder also failed a member. [`stale_index_only_end`] reads it.
    #[serde(default)]
    pub stale_index_only_pass_seq: u64,
    /// Per-program states, keyed by program name.
    #[serde(default)]
    pub programs: BTreeMap<String, ProgramStatus>,
    /// Every top-level key this build does not know, carried verbatim through each rewrite,
    /// so builds sharing a store never erase each other's fields.
    #[serde(flatten)]
    pub extra: BTreeMap<String, aterm_toml::Value>,
}

/// Stamp the index build the pass resolved: `last_index_build` and `index_build_changed_at`
/// move when `index_build` differs from the recorded one (`None`, no index in hand, changes
/// neither); `updated_at` always moves. Seeded through [`seed_for_rewrite`].
///
/// # Errors
/// [`read_checked`]'s diagnostic for an unreadable record, or the write's.
pub fn stamp_index_build(layout: &Layout, now: &str, index_build: Option<u64>) -> io::Result<()> {
    let mut status = seed_for_rewrite(layout)?;
    if let Some(build) = index_build
        && build != status.last_index_build
    {
        status.last_index_build = build;
        status.index_build_changed_at = now.to_string();
    }
    status.updated_at = now.to_string();
    write(layout, &status)
}

/// Whether no index update pass has ever completed here: `status.toml` absent, unreadable,
/// or with an empty [`Status::last_success_at`] (a record a failed resolve wrote is not a
/// check that ran). Test-only.
#[cfg(test)]
#[must_use]
pub(crate) fn never_checked(layout: &Layout) -> bool {
    read(layout).is_none_or(|s| s.last_success_at.trim().is_empty())
}

/// Stamp a full pass that reached the signed index and ran to its end: `last_success_at`
/// and `updated_at`, creating a minimal record when none exists. The one event that makes
/// "packages can be updated on this machine" true; a member that failed inside such a pass
/// is recorded in its own row. Seeded through [`seed_for_rewrite`].
///
/// # Errors
/// [`read_checked`]'s diagnostic for an unreadable record, or the write's.
pub fn stamp_success(layout: &Layout, now: &str) -> io::Result<()> {
    let mut status = seed_for_rewrite(layout)?;
    status.last_success_at = now.to_string();
    status.updated_at = now.to_string();
    write(layout, &status)
}

/// Stamp how a full `update` pass ended: `last_pass` (the outcome's word), `last_pass_at`
/// and `updated_at`. A pass that did not reach the signed index records `failed` or
/// `offline` and stamps no success, so the walk's interval and "never checked" count from
/// the last pass that did. This function and its `_with_index`/`_recorded` forms are the
/// one writer of those keys, called once at the pass's end (`cli`'s recorded pass); the
/// schedulers read them through [`aterm_update_core::pkg_check::Stamps`].
///
/// # Errors
/// [`read_checked`]'s diagnostic for an unreadable record, or the write's.
pub fn stamp_pass_end(
    layout: &Layout,
    now: &str,
    outcome: aterm_update_core::pkg_check::PassOutcome,
) -> io::Result<()> {
    stamp_pass_end_with_index(layout, now, outcome, 0)
}

/// [`stamp_pass_end`] with this pass's target or resolved index, in the same write as the
/// outcome and end time. An unknown target (0) clears the previous pass's witness.
///
/// # Errors
/// [`read_checked`]'s diagnostic for an unreadable record, or the write's.
pub fn stamp_pass_end_with_index(
    layout: &Layout,
    now: &str,
    outcome: aterm_update_core::pkg_check::PassOutcome,
    attempted_index_build: u64,
) -> io::Result<()> {
    stamp_pass_end_recorded(layout, now, outcome, attempted_index_build, false)
}

/// [`stamp_pass_end_with_index`], also recording whether this pass failed only because its
/// index came from the cache (`stale_index_only`, [`Status::stale_index_only_pass_seq`]),
/// in the same write as the end it describes.
///
/// # Errors
/// [`read_checked`]'s diagnostic for an unreadable record, or the write's.
pub fn stamp_pass_end_recorded(
    layout: &Layout,
    now: &str,
    outcome: aterm_update_core::pkg_check::PassOutcome,
    attempted_index_build: u64,
    stale_index_only: bool,
) -> io::Result<()> {
    let mut status = seed_for_rewrite(layout)?;
    status.last_pass = outcome.word().to_string();
    // Older binaries write whole-second `last_pass_at` and keep unknown fields in `extra`;
    // the `.000000000` suffix marks this writer (a format marker, not subsecond precision),
    // so an old pass ending in the same second cannot inherit this target.
    let witness_at = (attempted_index_build > 0)
        .then(|| now.strip_suffix('Z'))
        .flatten()
        .filter(|stamp| !stamp.contains('.'))
        .map(|stamp| format!("{stamp}.000000000Z"));
    status.last_pass_at = witness_at.clone().unwrap_or_else(|| now.to_string());
    status.pass_seq = status.pass_seq.saturating_add(1);
    status.stale_index_only_pass_seq =
        if stale_index_only && outcome == aterm_update_core::pkg_check::PassOutcome::Failed {
            status.pass_seq
        } else {
            0
        };
    status.last_pass_attempted_index_build = attempted_index_build;
    status.last_pass_attempted_at = witness_at.unwrap_or_default();
    status.updated_at = now.to_string();
    write(layout, &status)
}

/// Whether `status`'s last recorded full pass ended `failed` only because its index came
/// from the cache ([`Status::stale_index_only_pass_seq`] naming the current
/// [`Status::pass_seq`]). Pure.
#[must_use]
pub fn stale_index_only_end(status: &Status) -> bool {
    status.pass_seq > 0
        && status.stale_index_only_pass_seq == status.pass_seq
        && status.last_pass == aterm_update_core::pkg_check::PassOutcome::Failed.word()
}

/// The machine-wide stamps every lane schedules a full pass on (the window's gate and walk,
/// the session lane, a window's launch), read now: `status.toml`'s, with the index build
/// raised to the one verified here ([`crate::index_probe::verified_floor`], so an index a
/// sibling landed counts) and whether a pass is installing now
/// ([`crate::progress::pass_running`]).
#[must_use]
pub fn pass_stamps(layout: &Layout, now_unix: i64) -> aterm_update_core::pkg_check::Stamps {
    let stamps = aterm_update_core::pkg_check::Stamps::read(&layout.status());
    aterm_update_core::pkg_check::Stamps {
        last_index_build: stamps
            .last_index_build
            .max(crate::index_probe::verified_floor(layout)),
        in_flight: crate::progress::pass_running(layout, u64::try_from(now_unix).unwrap_or(0)),
        ..stamps
    }
}

/// How many full passes have recorded their end on this store ([`Status::pass_seq`]);
/// zero with no readable record.
#[must_use]
pub fn pass_seq(layout: &Layout) -> u64 {
    read(layout).map_or(0, |status| status.pass_seq)
}

impl Status {
    /// Serialize to TOML.
    ///
    /// # Errors
    /// The serializer's message, prefixed, when the map cannot be rendered.
    pub fn to_toml(&self) -> Result<String, String> {
        aterm_toml::to_string(self).map_err(|e| format!("serialize status: {e}"))
    }
}

/// Write `status` to `layout.status()`, durably ([`write_durably`]) — except while a
/// mutating verb holds the store lock ([`hold`]) and `status` differs from what is on disk
/// only in its stamps ([`only_stamps_differ`]): that lands in the held record, written
/// once when the hold ends. Best-effort: a failure is returned but is never fatal to an
/// apply (status is diagnostics). The bounds are checked here either way, so a held write
/// refuses what the final write would.
pub fn write(layout: &Layout, status: &Status) -> io::Result<()> {
    let text = render(status)?;
    // The record as it stood, for the package log's transitions: the held record while a
    // verb holds the lock, else the file. Every row writer comes through here, so this is
    // the one place a transition is seen; an unreadable record is no baseline.
    let before = crate::packages_log::records(layout)
        .then(|| read_checked(layout).ok())
        .flatten();
    match held::store(layout, status) {
        Some(false) => {}
        Some(true) => {
            write_durably(layout, &text)?;
            held::settle(layout, status);
        }
        None => write_durably(layout, &text)?,
    }
    if let Some(before) = before {
        crate::packages_log::note_rows(layout, before.as_ref(), status);
    }
    Ok(())
}

/// Whether `a` and `b` differ only in what a pass stamps as it goes (the outcome sentence
/// and the clocks), which is all a hold coalesces: a lost stamp is re-stamped by the next
/// pass, while a lost row (an activated build and its signed root, a seam, a tombstone)
/// lets the next pass record a new build under an old root, which `verify` calls drift.
fn only_stamps_differ(a: &Status, b: &Status) -> bool {
    let unstamped = |s: &Status| Status {
        updated_at: String::new(),
        outcome: String::new(),
        last_success_at: String::new(),
        last_index_build: 0,
        index_build_changed_at: String::new(),
        last_pass: String::new(),
        last_pass_at: String::new(),
        last_pass_attempted_index_build: 0,
        last_pass_attempted_at: String::new(),
        pass_seq: 0,
        stale_index_only_pass_seq: 0,
        ..s.clone()
    };
    unstamped(a) == unstamped(b)
}

/// `status` as the bytes [`write`] puts on disk, or why it may not be written.
fn render(status: &Status) -> io::Result<String> {
    if status.programs.len() > MAX_STATUS_PROGRAMS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "status.toml has {} programs; limit is {MAX_STATUS_PROGRAMS}",
                status.programs.len()
            ),
        ));
    }
    let text = status
        .to_toml()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    if text.len() > MAX_STATUS_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("status.toml exceeds the {MAX_STATUS_BYTES}-byte limit"),
        ));
    }
    Ok(text)
}

/// Put `text` at `layout.status()` so a power loss leaves the old record or this one: a
/// temp beside it, `fsync`, `rename(2)`, then a directory sync. The temp is removed on
/// every failure arm: nothing sweeps `status.toml.tmp-<pid>`, so a full disk would strand
/// one per pass.
fn write_durably(layout: &Layout, text: &str) -> io::Result<()> {
    let dest = layout.status();
    let tmp = dest.with_file_name(format!("status.toml.tmp-{}", std::process::id()));
    let written = (|| {
        use std::io::Write as _;
        let mut f = crate::platform::open_create_write(&tmp, 0o644)?;
        f.write_all(text.as_bytes())?;
        crate::store::sync_contents_or_accept_refusal(&f)?;
        drop(f);
        std::fs::rename(&tmp, &dest)
    })();
    if let Err(error) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(error);
    }
    crate::store::sync_dir(&layout.prefix);
    #[cfg(test)]
    counted::note(layout);
    Ok(())
}

/// How many times each store's record reached the disk in this test process — the
/// instrument behind "one write per pass".
#[cfg(test)]
pub(crate) mod counted {
    use super::{Layout, PathBuf};

    static WRITES: super::Mutex<Vec<(PathBuf, usize)>> = super::Mutex::new(Vec::new());

    pub(super) fn note(layout: &Layout) {
        let mut w = WRITES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match w.iter_mut().find(|(p, _)| *p == layout.prefix) {
            Some((_, n)) => *n += 1,
            None => w.push((layout.prefix.clone(), 1)),
        }
    }

    /// The durable writes of `layout`'s record so far.
    #[cfg(unix)]
    pub(crate) fn writes(layout: &Layout) -> usize {
        let w = WRITES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        w.iter()
            .find(|(p, _)| *p == layout.prefix)
            .map_or(0, |(_, n)| *n)
    }
}

/// Read + parse `status.toml` with explicit admission/parse diagnostics — the held record
/// while this process holds one for `layout` ([`hold`]).
///
/// Missing is a normal `Ok(None)`. Existing paths must be bounded regular
/// non-link files containing valid UTF-8 TOML and no more than
/// [`MAX_STATUS_PROGRAMS`] rows.
pub fn read_checked(layout: &Layout) -> io::Result<Option<Status>> {
    if let Some(held) = held::load(layout) {
        return held;
    }
    read_disk(layout)
}

/// [`read_checked`] of the file itself.
fn read_disk(layout: &Layout) -> io::Result<Option<Status>> {
    let path = layout.status();
    let text = match crate::metadata_io::read_bounded_regular_utf8(&path, MAX_STATUS_BYTES) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let status: Status = aterm_toml::from_str(&text).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("status.toml is invalid: {error}"),
        )
    })?;
    if status.programs.len() > MAX_STATUS_PROGRAMS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "status.toml has {} programs; limit is {MAX_STATUS_PROGRAMS}",
                status.programs.len()
            ),
        ));
    }
    Ok(Some(status))
}

/// Compatibility projection: absent or malformed diagnostics yield `None`.
/// Interactive surfaces which can show an error should use [`read_checked`].
#[must_use]
pub fn read(layout: &Layout) -> Option<Status> {
    read_checked(layout).ok().flatten()
}

/// Seed a writer that rebuilds the whole record: the record on disk, a minimal schema-1 one
/// when the store has none, or, when the record is corrupt, one rebuilt from the store with
/// the corrupt bytes kept aside ([`heal`]). Refusing a corrupt record would block every
/// later stamp, so it is healed.
///
/// # Errors
/// The record cannot be read for a reason that is not corruption (a permission, an I/O
/// error, a descriptor limit; it is left as it is), or it is corrupt and could not be kept
/// aside.
pub fn seed_for_rewrite(layout: &Layout) -> io::Result<Status> {
    match read_checked(layout) {
        Ok(Some(status)) => Ok(status),
        Ok(None) => Ok(Status {
            schema: 1,
            ..Default::default()
        }),
        Err(why) => heal(layout, why),
    }
}

/// Whether the read error `why` means the record itself is bad, rather than that this read
/// failed: bytes that do not parse, are not UTF-8 or exceed the bounds (`InvalidData`), or
/// a path that is not a regular file. Anything else (`EACCES`, `EIO`, `EMFILE`, …) may pass
/// by the next read, and a healthy record must never be moved aside for it.
fn is_corrupt(layout: &Layout, why: &io::Error) -> bool {
    why.kind() == io::ErrorKind::InvalidData
        || std::fs::symlink_metadata(layout.status()).is_ok_and(|m| !m.file_type().is_file())
}

/// Replace a corrupt record with [`rebuilt_from_store`]'s, keeping the corrupt one beside
/// it as `status.toml.corrupt-<unix>`, said once as a notice. A regular file's bytes are
/// linked (or copied) aside and the rebuilt record renamed over it, so a kill in between
/// leaves the old bytes or the new record, never no record. `Err` (nothing replaced) when
/// `why` is not corruption ([`is_corrupt`]) or the bytes could not be kept.
fn heal(layout: &Layout, why: io::Error) -> io::Result<Status> {
    if !is_corrupt(layout, &why) {
        return Err(why);
    }
    let rebuilt = rebuilt_from_store(layout);
    let text = render(&rebuilt)?;
    let path = layout.status();
    let aside = corrupt_path(layout, u64::try_from(crate::flow::now_unix()).unwrap_or(0));
    if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_file()) {
        std::fs::hard_link(&path, &aside).or_else(|_| std::fs::copy(&path, &aside).map(|_| ()))?;
    } else {
        std::fs::rename(&path, &aside)?;
    }
    write_durably(layout, &text)?;
    held::settle(layout, &rebuilt);
    crate::notice::say(&format!(
        "{} could not be read ({why}) — kept as {} and rebuilt from the store",
        path.display(),
        aside.display()
    ));
    Ok(rebuilt)
}

/// `status.toml.corrupt-<unix>`, or `…-<unix>-<pid>` when a record was already kept aside
/// in the same second.
fn corrupt_path(layout: &Layout, unix: u64) -> PathBuf {
    let first = layout.prefix.join(format!("status.toml.corrupt-{unix}"));
    if std::fs::symlink_metadata(&first).is_err() {
        return first;
    }
    layout
        .prefix
        .join(format!("status.toml.corrupt-{unix}-{}", std::process::id()))
}

/// The record the store describes when the written one is lost: a row per active program
/// ([`store_row`]), and the rustup seams that point into this prefix. What the store
/// cannot say — the signed roots, the stamps — the next pass records again
/// (`cli::recover_missing_roots` re-derives the roots).
#[must_use]
pub fn rebuilt_from_store(layout: &Layout) -> Status {
    let programs = crate::active_builds(layout)
        .into_iter()
        .map(|(program, build)| {
            let row = store_row(layout, &program, build);
            (program, row)
        })
        .collect();
    Status {
        schema: 1,
        seams: crate::seam::attached_keys(layout),
        programs,
        ..Default::default()
    }
}

/// The row the store itself can vouch for, for `program` active at `build`: a vendor build
/// by its recorded version, an index build as `active` (a word every reader accepts; the
/// next pass rewrites it to its pin), and no signed root (the store holds no signature).
#[must_use]
pub fn store_row(layout: &Layout, program: &str, build: u64) -> ProgramStatus {
    let vendor = crate::vendor_direct::spec(program).and_then(|spec| {
        crate::vendor_direct::complete_record(&layout.build_dir(program, build))
            .map(|record| crate::state::vendor_source(&record.version.to_string(), spec.vendor))
    });
    ProgramStatus {
        installed_build: Some(build),
        state: vendor.unwrap_or_else(|| String::from("active")),
        tree_root: String::new(),
    }
}

/// `status` as a shell whose `PATH` is `path_var` sees it: a managed row of an active
/// program one of whose tools a foreign executable out-ranks on that `PATH` reads `managed
/// <build> — SHADOWED by <path>`. Shadowing is per-shell, so passes do not record it;
/// `doctor`, `which` and the Settings page compute it when they read. An agent program
/// whose `agents/` twin is current (every aterm tab puts `agents/` first) and a dev-linked
/// program are never shadowed, and a row already recorded as SHADOWED is left as written.
#[must_use]
pub fn with_shadows(
    layout: &Layout,
    mut status: Status,
    path_var: Option<&std::ffi::OsStr>,
) -> Status {
    let scan = crate::ops::BinScan::capture(layout);
    for (program, build) in crate::ops::active_builds_in(&scan) {
        let Some(row) = status.programs.get_mut(&program) else {
            continue;
        };
        if row.installed_build != Some(build)
            || !crate::state::is_managed(&row.state)
            || crate::state::shadowed_by(&row.state).is_some()
            || crate::linkmode::is_linked(layout, &program)
        {
            continue;
        }
        let shadow = crate::ops::active_tools_in(&scan, &program, build)
            .into_iter()
            .find_map(|tool| {
                crate::vendor::shadowing_binary_on_path(&layout.prefix, tool.as_str(), path_var)
            });
        if let Some(path) = shadow
            && !(crate::stub::is_agent_program(&program)
                && crate::cli::agent_twin_current(layout, &program, build))
        {
            row.state = crate::state::shadowed(build, &path);
        }
    }
    status
}

/// Hold `layout`'s stamps in memory until the returned guard drops, and write them once
/// then, so a mutating verb writes `status.toml` once however often it moves the outcome
/// and the clocks. Every other change is written through at once ([`write`]): atpkg has no
/// signal handler, so a pass killed by Ctrl-C, a logout or SIGKILL must already have its
/// rows on disk. Nests. The CLI takes it right after the store lock, so the record is on
/// disk before the lock is released.
#[must_use]
pub fn hold(layout: &Layout) -> HeldRecord {
    held::begin(layout);
    HeldRecord {
        layout: layout.clone(),
    }
}

/// The guard [`hold`] returns.
pub struct HeldRecord {
    layout: Layout,
}

impl Drop for HeldRecord {
    fn drop(&mut self) {
        if let Some(status) = held::end(&self.layout)
            && let Ok(text) = render(&status)
        {
            let _ = write_durably(&self.layout, &text);
        }
    }
}

/// The in-memory records of the stores this process holds ([`hold`]), keyed by prefix.
mod held {
    use super::{Layout, PathBuf, Status, io};

    /// One held store: `record` is the record as it stands and `disk` as the file holds
    /// it — each `None` until first read or written (`Some(None)`: the store has none).
    struct Held {
        prefix: PathBuf,
        depth: u32,
        record: Option<Option<Status>>,
        disk: Option<Option<Status>>,
        dirty: bool,
    }

    static HELD: super::Mutex<Vec<Held>> = super::Mutex::new(Vec::new());

    fn with<T>(layout: &Layout, f: impl FnOnce(&mut Held) -> T) -> Option<T> {
        let mut held = HELD
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held.iter_mut().find(|h| h.prefix == layout.prefix).map(f)
    }

    pub(super) fn begin(layout: &Layout) {
        let mut held = HELD
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match held.iter_mut().find(|h| h.prefix == layout.prefix) {
            Some(h) => h.depth += 1,
            None => held.push(Held {
                prefix: layout.prefix.clone(),
                depth: 1,
                record: None,
                disk: None,
                dirty: false,
            }),
        }
    }

    /// The outermost end: the record to write, if it holds anything the disk does not.
    pub(super) fn end(layout: &Layout) -> Option<Status> {
        let mut held = HELD
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let at = held.iter().position(|h| h.prefix == layout.prefix)?;
        held[at].depth -= 1;
        if held[at].depth > 0 {
            return None;
        }
        let h = held.swap_remove(at);
        if h.dirty { h.record.flatten() } else { None }
    }

    /// The held record, loaded from disk on first use; `None` when `layout` is not held.
    /// An unreadable file is not cached: the writer that heals it settles what replaces it.
    pub(super) fn load(layout: &Layout) -> Option<io::Result<Option<Status>>> {
        with(layout, |h| {
            if let Some(record) = &h.record {
                return Ok(record.clone());
            }
            let disk = super::read_disk(layout)?;
            h.record = Some(disk.clone());
            h.disk = Some(disk.clone());
            Ok(disk)
        })
    }

    /// Take `status` into the held record: `Some(true)` when it must reach the disk now —
    /// it differs from the file in more than its stamps, or the file is not known — and
    /// `Some(false)` when it may wait for the end; `None` when `layout` is not held.
    pub(super) fn store(layout: &Layout, status: &Status) -> Option<bool> {
        with(layout, |h| {
            let through = match &h.disk {
                Some(Some(on_disk)) => !super::only_stamps_differ(on_disk, status),
                _ => true,
            };
            h.record = Some(Some(status.clone()));
            h.dirty = true;
            through
        })
    }

    /// `status` is what the file now holds (written through, or healed).
    pub(super) fn settle(layout: &Layout, status: &Status) {
        with(layout, |h| {
            h.record = Some(Some(status.clone()));
            h.disk = Some(Some(status.clone()));
            h.dirty = false;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn layout(label: &str) -> crate::testdir::ScratchLayout {
        crate::testdir::layout(&format!("atpkg-status-{label}-"))
    }

    /// A pass that reached the index stamps its success and ends `ok`; one that did not ends
    /// `failed` and keeps the success; the build fields move only on a different build.
    #[test]
    fn the_pass_end_stamps_say_what_the_pass_did() {
        use aterm_update_core::pkg_check::{PassOutcome, Stamps};
        let dir = crate::testdir::dir("atpkg-status-fresh-");
        let layout = Layout {
            prefix: dir.clone(),
        };
        stamp_success(&layout, "2026-09-15T10:00:00Z").unwrap();
        stamp_pass_end(&layout, "2026-09-15T10:00:00Z", PassOutcome::Ok).unwrap();
        stamp_index_build(&layout, "2026-09-15T10:00:00Z", Some(32)).unwrap();
        let s = read(&layout).unwrap();
        assert_eq!(s.last_success_at, "2026-09-15T10:00:00Z");
        assert_eq!(s.last_pass, "ok");
        assert_eq!(s.last_index_build, 32);
        assert_eq!(s.index_build_changed_at, "2026-09-15T10:00:00Z");
        assert!(!Stamps::read(&layout.status()).last_failed());
        // A pass that did not reach the index: it ends `failed`, the success stays.
        stamp_pass_end(&layout, "2026-09-16T10:00:00Z", PassOutcome::Failed).unwrap();
        stamp_index_build(&layout, "2026-09-16T10:00:00Z", Some(32)).unwrap();
        let s = read(&layout).unwrap();
        assert_eq!(s.last_success_at, "2026-09-15T10:00:00Z");
        assert_eq!(s.last_pass, "failed");
        assert_eq!(s.last_pass_at, "2026-09-16T10:00:00Z");
        assert_eq!(s.index_build_changed_at, "2026-09-15T10:00:00Z");
        assert_eq!(s.updated_at, "2026-09-16T10:00:00Z");
        assert!(Stamps::read(&layout.status()).last_failed());
        // A pass that reached it on a newer build: `ok` again, both build fields move.
        stamp_success(&layout, "2026-09-17T10:00:00Z").unwrap();
        stamp_pass_end(&layout, "2026-09-17T10:00:00Z", PassOutcome::Ok).unwrap();
        stamp_index_build(&layout, "2026-09-17T10:00:00Z", Some(33)).unwrap();
        let s = read(&layout).unwrap();
        assert_eq!(s.last_success_at, "2026-09-17T10:00:00Z");
        assert_eq!(s.last_pass, "ok");
        assert!(
            !Stamps::read(&layout.status()).last_failed(),
            "the success heals"
        );
        assert_eq!(s.last_index_build, 33);
        assert_eq!(s.index_build_changed_at, "2026-09-17T10:00:00Z");
        // No index in hand: the build fields are untouched.
        stamp_index_build(&layout, "2026-09-18T10:00:00Z", None).unwrap();
        let s = read(&layout).unwrap();
        assert_eq!(s.last_index_build, 33);
        assert_eq!(s.index_build_changed_at, "2026-09-17T10:00:00Z");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pass_end_binds_its_index_target_and_clears_unknown_successors() {
        use aterm_update_core::pkg_check::{PassOutcome, Stamps};
        let l = layout("pass-target");
        stamp_index_build(&l, "2026-09-24T12:00:00Z", Some(44)).unwrap();
        stamp_pass_end_with_index(&l, "2026-09-24T12:01:00Z", PassOutcome::Ok, 45).unwrap();
        let status = read(&l).unwrap();
        assert_eq!(status.last_index_build, 44);
        assert_eq!(status.last_pass_attempted_index_build, 45);
        assert_eq!(status.last_pass_at, status.last_pass_attempted_at);
        assert!(status.last_pass_at.ends_with(".000000000Z"));
        let stamps = Stamps::read(&l.status());
        assert_eq!(stamps.last_pass_target.map(|(_, build)| build), Some(45));
        assert!(!stamps.completed_older_index_pass(45));
        assert!(stamps.completed_older_index_pass(46));

        // An older binary rewrites `last_pass_at` and carries the new fields through
        // `extra`: an old pass ending in the same second must not inherit this target.
        let mut old_writer = status;
        old_writer.last_pass = "failed".to_string();
        old_writer.last_pass_at = "2026-09-24T12:01:00Z".to_string();
        write(&l, &old_writer).unwrap();
        let stale = Stamps::read(&l.status());
        assert_eq!(stale.last_pass_target, None);
        assert!(!stale.completed_older_index_pass(46));

        stamp_pass_end(&l, "2026-09-24T12:02:00Z", PassOutcome::Failed).unwrap();
        let next = Stamps::read(&l.status());
        assert_eq!(read(&l).unwrap().last_pass_attempted_index_build, 0);
        assert_eq!(
            next.last_pass_target, None,
            "the old target is not borrowed"
        );
        assert!(!next.completed_older_index_pass(46));
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A failed write reclaims its `status.toml.tmp-<pid>`, which nothing else sweeps.
    /// Staged with a `status.toml` that is a directory, so the rename is what refuses.
    #[cfg(unix)]
    #[test]
    fn failed_write_reclaims_its_temp() {
        let l = layout("failed-write");
        std::fs::create_dir(l.status()).unwrap();
        let error = write(
            &l,
            &Status {
                schema: 1,
                ..Default::default()
            },
        )
        .expect_err("a rename onto a directory cannot succeed");
        // EISDIR (21 on every unix this ships to): the bytes reached the temp and the rename
        // refused, not an earlier step.
        assert_eq!(
            error.raw_os_error(),
            Some(21),
            "the staged failure must be the rename's, not an earlier one: {error}"
        );
        let leftovers: Vec<_> = std::fs::read_dir(&l.prefix)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .map(|e| e.file_name())
            .collect();
        assert!(
            leftovers.is_empty(),
            "a failed status write must reclaim its temp, not strand it: {leftovers:?}"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn read_absent_or_corrupt_is_none() {
        let l = layout("absent");
        assert!(read(&l).is_none(), "absent status reads as None");
        assert_eq!(read_checked(&l).unwrap(), None);
        std::fs::write(l.status(), "this is not valid toml {{{").unwrap();
        assert!(read(&l).is_none(), "corrupt status is never load-bearing");
        let error = read_checked(&l).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("status.toml is invalid"));
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A rewriting writer heals an unreadable record (garbage, truncated, empty) by keeping
    /// the bytes aside and rebuilding from the store, and its stamp lands; an absent record
    /// seeds a fresh one and keeps nothing aside.
    #[test]
    fn rewriting_writers_heal_an_unreadable_record() {
        let l = layout("unreadable");
        assert_eq!(
            seed_for_rewrite(&l).unwrap().schema,
            1,
            "a virgin store seeds a fresh record: absent is not unreadable"
        );
        let aside = |l: &Layout| -> Vec<String> {
            let mut v: Vec<String> = std::fs::read_dir(&l.prefix)
                .unwrap()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with("status.toml.corrupt-"))
                .collect();
            v.sort();
            v
        };
        assert!(aside(&l).is_empty());
        let whole = "schema = 1\nupdated_at = \"2026-09-16T00:00:00Z\"\nenabled = true\n\
                     index_source = \"o/r\"\noutcome = \"up to date\"\n";
        for (label, bytes) in [
            (
                "garbage",
                "schema = 1\nseams = [\"rustup:trust\"]\nthis is not valid toml {{{\n",
            ),
            ("truncated", &whole[..whole.len() / 2]),
            ("empty", ""),
        ] {
            std::fs::write(l.status(), bytes).unwrap();
            assert!(read_checked(&l).is_err(), "{label}: unreadable as written");
            stamp_success(&l, "2026-09-16T00:00:00Z").expect("the stamp heals and lands");
            assert!(!never_checked(&l), "{label}: the stamp landed");
            let kept = aside(&l);
            let newest = kept.last().expect("kept aside");
            assert_eq!(
                std::fs::read_to_string(l.prefix.join(newest)).unwrap(),
                bytes,
                "{label}: the unreadable bytes are kept, byte for byte"
            );
            for name in kept {
                std::fs::remove_file(l.prefix.join(name)).unwrap();
            }
        }
        // A record rebuilt from the store names what the store holds.
        stamp_index_build(&l, "2026-09-16T00:00:00Z", Some(7)).unwrap();
        assert_eq!(read(&l).unwrap().last_index_build, 7);
        // Healed in place even inside a hold: the rebuilt record is on disk as soon as the
        // corrupt one is kept aside, never "no record" for the length of a pass.
        std::fs::write(l.status(), "garbage {{{").unwrap();
        {
            let _held = hold(&l);
            stamp_success(&l, "2026-09-17T00:00:00Z").unwrap();
            let on_disk = read_disk(&l).expect("a rebuilt record is on disk");
            assert_eq!(on_disk.map(|s| s.schema), Some(1));
        }
        assert_eq!(read(&l).unwrap().last_success_at, "2026-09-17T00:00:00Z");
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A read that failed (a permission, an I/O error) is not corruption: the healthy record
    /// is left where it is, never moved aside or replaced.
    #[cfg(unix)]
    #[test]
    fn a_record_that_could_not_be_read_is_left_alone() {
        let l = layout("unreadable-perm");
        stamp_success(&l, "2026-09-16T00:00:00Z").unwrap();
        std::fs::set_permissions(l.status(), std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(l.status()).is_ok() {
            // Running as root: a mode cannot refuse this read.
            let _ = std::fs::remove_dir_all(&l.prefix);
            return;
        }
        let error = stamp_success(&l, "2026-09-17T00:00:00Z").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{error}");
        std::fs::set_permissions(l.status(), std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(read(&l).unwrap().last_success_at, "2026-09-16T00:00:00Z");
        let aside = std::fs::read_dir(&l.prefix)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".corrupt-"))
            .count();
        assert_eq!(aside, 0, "nothing kept aside");
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// Keys another build wrote (a field, a table) survive an older writer's full rebuild.
    #[test]
    fn an_unknown_field_survives_a_rewrite() {
        let l = layout("unknown-field");
        let newer = "schema = 1\nupdated_at = \"2026-09-16T00:00:00Z\"\nenabled = true\n\
                     index_source = \"o/r\"\noutcome = \"up to date\"\n\
                     a_future_field = \"kept\"\na_future_count = 7\n\n\
                     [a_future_table]\nnested = true\n\n\
                     [programs.ay]\ninstalled_build = 18\nstate = \"active\"\n";
        std::fs::write(l.status(), newer).unwrap();
        stamp_success(&l, "2026-09-17T00:00:00Z").unwrap();
        let back = read(&l).unwrap();
        assert_eq!(back.last_success_at, "2026-09-17T00:00:00Z");
        assert_eq!(
            back.extra
                .get("a_future_field")
                .and_then(aterm_toml::Value::as_str),
            Some("kept")
        );
        assert_eq!(
            back.extra
                .get("a_future_count")
                .and_then(aterm_toml::Value::as_integer),
            Some(7)
        );
        assert!(
            back.extra.contains_key("a_future_table"),
            "{:?}",
            back.extra
        );
        assert!(
            !back.extra.contains_key("programs"),
            "known keys are never extra"
        );
        assert_eq!(back.programs["ay"].installed_build, Some(18));
        let text = std::fs::read_to_string(l.status()).unwrap();
        assert!(text.contains("a_future_field = \"kept\""), "{text}");
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// While a store is held the stamps land in memory, where readers in this process see
    /// them; the file is written once when the hold ends, and not at all if nothing changed.
    #[cfg(unix)]
    #[test]
    fn a_held_record_is_written_once_when_the_hold_ends() {
        use std::os::unix::fs::MetadataExt as _;
        let l = layout("held");
        stamp_success(&l, "2026-09-16T00:00:00Z").unwrap();
        let ino = std::fs::metadata(l.status()).unwrap().ino();
        {
            let _held = hold(&l);
            let _inner = hold(&l);
            stamp_success(&l, "2026-09-17T00:00:00Z").unwrap();
            stamp_index_build(&l, "2026-09-17T00:00:00Z", Some(9)).unwrap();
            let mut s = seed_for_rewrite(&l).unwrap();
            s.outcome = "up to date (index build 9)".into();
            write(&l, &s).unwrap();
            assert_eq!(
                read(&l).unwrap().last_index_build,
                9,
                "readers see the held record"
            );
            assert_eq!(
                std::fs::metadata(l.status()).unwrap().ino(),
                ino,
                "nothing on disk yet"
            );
        }
        let back = read(&l).unwrap();
        assert_eq!(back.last_success_at, "2026-09-17T00:00:00Z");
        assert_eq!(back.last_index_build, 9);
        assert_eq!(back.outcome, "up to date (index build 9)");
        let written = std::fs::metadata(l.status()).unwrap().ino();
        assert_ne!(written, ino, "written once, at the end");
        {
            let _held = hold(&l);
            let _ = read(&l);
        }
        assert_eq!(
            std::fs::metadata(l.status()).unwrap().ino(),
            written,
            "a hold nothing wrote into writes nothing"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A row change is written through even while held: a pass killed after activating a
    /// build must leave its row and signed root on disk. Only the stamps after it wait.
    #[cfg(unix)]
    #[test]
    fn a_held_row_change_reaches_the_disk_at_once() {
        use std::os::unix::fs::MetadataExt as _;
        let l = layout("held-row");
        stamp_success(&l, "2026-09-16T00:00:00Z").unwrap();
        let held = hold(&l);
        let mut s = seed_for_rewrite(&l).unwrap();
        s.programs.insert(
            "ay".into(),
            ProgramStatus {
                installed_build: Some(19),
                state: "managed 19 — pinned by index 44".into(),
                tree_root: "r19".into(),
            },
        );
        s.outcome = "installed ay build 19".into();
        write(&l, &s).unwrap();
        let on_disk = read_disk(&l).unwrap().unwrap();
        assert_eq!(
            on_disk.programs["ay"].tree_root, "r19",
            "the row is on disk"
        );
        let ino = std::fs::metadata(l.status()).unwrap().ino();
        stamp_success(&l, "2026-09-17T00:00:00Z").unwrap();
        assert_eq!(
            std::fs::metadata(l.status()).unwrap().ino(),
            ino,
            "a stamp after it waits"
        );
        // The kill: the guard never drops. What the disk holds is the row, not the stamp.
        std::mem::forget(held);
        let on_disk = read_disk(&l).unwrap().unwrap();
        assert_eq!(on_disk.programs["ay"].installed_build, Some(19));
        assert_eq!(on_disk.last_success_at, "2026-09-16T00:00:00Z");
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn status_program_count_is_bounded_on_read_and_write() {
        let l = layout("program-cap");
        let mut programs = BTreeMap::new();
        for index in 0..=MAX_STATUS_PROGRAMS {
            programs.insert(format!("p{index}"), ProgramStatus::default());
        }
        let status = Status {
            schema: 1,
            programs,
            ..Default::default()
        };
        let write_error = write(&l, &status).unwrap_err();
        assert_eq!(write_error.kind(), io::ErrorKind::InvalidData);
        assert!(write_error.to_string().contains("limit is"));

        // Bypass the writer to prove a hostile otherwise-valid on-disk record
        // is rejected by the independent read-side count gate too.
        std::fs::write(l.status(), status.to_toml().unwrap()).unwrap();
        let read_error = read_checked(&l).unwrap_err();
        assert_eq!(read_error.kind(), io::ErrorKind::InvalidData);
        assert!(read_error.to_string().contains("limit is"));
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// The `seams` record round-trips beside the program rows (aterm-toml orders values
    /// before sub-tables, so the array never lands inside the last `[programs.*]` table),
    /// and a record without the field parses as no seams.
    #[test]
    fn seams_round_trip_and_default_empty() {
        let l = layout("seams");
        let mut programs = BTreeMap::new();
        programs.insert(
            "trust".to_string(),
            ProgramStatus {
                installed_build: Some(6808),
                state: "managed 6808 — pinned by index 9".into(),
                tree_root: "r".into(),
            },
        );
        programs.insert("ay".to_string(), ProgramStatus::default());
        let s = Status {
            schema: 1,
            updated_at: "2026-08-29T00:00:00Z".into(),
            enabled: true,
            index_source: "o/r".into(),
            outcome: "up to date".into(),
            seams: vec!["rustup:trust".into()],
            last_success_at: String::new(),
            last_index_build: 0,
            index_build_changed_at: String::new(),
            last_pass: String::new(),
            last_pass_at: String::new(),
            last_pass_attempted_index_build: 0,
            last_pass_attempted_at: String::new(),
            pass_seq: 3,
            stale_index_only_pass_seq: 0,
            programs,
            extra: Default::default(),
        };
        write(&l, &s).unwrap();
        let back = read(&l).expect("status reads back");
        assert_eq!(back, s);
        assert_eq!(back.seams, vec!["rustup:trust".to_string()]);
        let text = std::fs::read_to_string(l.status()).unwrap();
        let seams_at = text.find("seams = [").expect("seams array on disk");
        let table_at = text.find("[programs.").expect("program tables on disk");
        assert!(
            seams_at < table_at,
            "seams must precede the program tables:\n{text}"
        );

        // A record without the `seams` key.
        let legacy = "schema = 1\nupdated_at = \"2026-08-01T00:00:00Z\"\nenabled = true\n\
             index_source = \"o/r\"\noutcome = \"up to date\"\n\n[programs.trust]\n\
             installed_build = 6808\nstate = \"managed 6808 — pinned by index 9\"\n\
             tree_root = \"r\"\n";
        std::fs::write(l.status(), legacy).unwrap();
        let back = read_checked(&l).unwrap().expect("legacy record parses");
        assert!(back.seams.is_empty(), "no key ⇒ no seams");
        assert_eq!(back.programs["trust"].installed_build, Some(6808));
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// "Never checked" is exactly `status.toml` absent or `last_success_at` empty: a record
    /// a failed pass wrote still reads as never checked; only [`stamp_success`] clears it.
    #[test]
    fn never_checked_holds_until_a_successful_pass_stamps_last_success_at() {
        let l = layout("never-checked");
        assert!(never_checked(&l), "no status.toml at all");
        // A failed pass's record: written, stamped, and still not a completed check.
        let mut programs = BTreeMap::new();
        programs.insert(
            "*index*".to_string(),
            ProgramStatus {
                installed_build: None,
                state: "error: index unreachable".into(),
                tree_root: String::new(),
            },
        );
        write(
            &l,
            &Status {
                schema: 1,
                updated_at: "2026-09-10T06:40:53Z".into(),
                outcome: "update failed: index unreachable".into(),
                programs,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(
            never_checked(&l),
            "a failed pass moves updated_at but is not a check that ran"
        );
        // A record from before the field existed parses as never checked too.
        let legacy = "schema = 1\nupdated_at = \"2026-08-01T00:00:00Z\"\nenabled = true\n\
             index_source = \"o/r\"\noutcome = \"up to date\"\n";
        std::fs::write(l.status(), legacy).unwrap();
        assert!(never_checked(&l), "pre-field record: empty last_success_at");
        // The success stamp — and only it — clears the condition, keeping the rest.
        stamp_success(&l, "2026-09-10T07:00:00Z").unwrap();
        assert!(!never_checked(&l));
        let back = read(&l).unwrap();
        assert_eq!(back.last_success_at, "2026-09-10T07:00:00Z");
        assert_eq!(back.updated_at, "2026-09-10T07:00:00Z");
        assert_eq!(back.outcome, "up to date", "the sentence is not touched");
        let text = std::fs::read_to_string(l.status()).unwrap();
        assert!(text.contains("last_success_at = \"2026-09-10T07:00:00Z\""));
        // Whitespace is not a stamp.
        std::fs::write(l.status(), "schema = 1\nlast_success_at = \"  \"\n").unwrap();
        assert!(never_checked(&l));
        // No record at all: the stamp creates a minimal one.
        std::fs::remove_file(l.status()).unwrap();
        stamp_success(&l, "2026-09-10T08:00:00Z").unwrap();
        assert!(!never_checked(&l));
        assert_eq!(read(&l).unwrap().schema, 1);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// Tier-1 of the derived model `AtpkgFullPassRule` (aterm-spec): every record the model
    /// reaches at `Buggy=0`, laid down by the real writers and read as the session lane reads
    /// it ([`pass_stamps`], then `full_pass_owed`), gives the model's `verdict` and pass in
    /// flight, and starts a pass exactly when `SessionLook` does. Negative control: the
    /// replaced reader ([`full_pass_rule_tier1::legacy_stamps`]) over `Buggy=1`'s records is
    /// `Buggy=1`'s lane, and holds back the pass owed after a head-watch write. The current
    /// reader over those pre-`last_pass` records reads no failure, and a writer that records
    /// no pass end reads a failed pass as none.
    #[test]
    fn the_full_pass_rule_conforms_to_its_derived_model() {
        use super::full_pass_rule_tier1::{legacy_stamps, looks, realize, records};
        use aterm_models_atpkg::atpkg_full_pass_rule_model;
        use aterm_update_core::pkg_check;
        let model = atpkg_full_pass_rule_model();
        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let fixed = records(&model);
        assert!(fixed.len() > 20, "{}", fixed.len());
        for (i, state) in fixed.iter().enumerate() {
            let now = crate::flow::now_unix();
            let (l, sink) = realize(&format!("tier1-{i}"), state, true, now);
            let stamps = pass_stamps(&l, now);
            let look = looks(&model, "SessionLook", state);
            assert_eq!(stamps.last_failed(), look["verdict"] == 1, "{state:?}");
            assert_eq!(stamps.in_flight, state["running"] >= 1, "{state:?}");
            assert_eq!(
                pkg_check::full_pass_owed(&stamps, now).is_some(),
                look["queued"] > 0,
                "{state:?} {stamps:?}"
            );
            drop(sink);
            let _ = std::fs::remove_dir_all(&l.prefix);
        }
        let old = records(&buggy);
        assert!(old.len() > 5, "{}", old.len());
        for (i, state) in old.iter().enumerate() {
            let now = crate::flow::now_unix();
            let (l, sink) = realize(&format!("tier1-old-{i}"), state, false, now);
            let legacy = legacy_stamps(&l, now);
            let look = looks(&buggy, "SessionLook", state);
            assert_eq!(legacy.last_failed(), look["verdict"] == 1, "{state:?}");
            assert_eq!(
                pkg_check::full_pass_owed(&legacy, now).is_some(),
                look["queued"] > 0,
                "{state:?} {legacy:?}"
            );
            let upgraded = pass_stamps(&l, now);
            assert!(!upgraded.last_failed(), "by its success alone: {state:?}");
            assert_eq!(
                pkg_check::full_pass_owed(&upgraded, now).is_some(),
                state["running"] == 0 && (state["ever_ok"] == 0 || state["ok_age"] >= 3),
                "{state:?} {upgraded:?}"
            );
            drop(sink);
            let _ = std::fs::remove_dir_all(&l.prefix);
        }
        // The replayed defect: a success an interval old, then the head watch writes.
        let defect = super::full_pass_rule_tier1::walk(
            &model,
            &[
                "SessionLook",
                "TakeLockFresh",
                "PassSucceeds",
                "Tick",
                "Tick",
                "Tick",
                "OtherWrite",
            ],
        );
        let now = crate::flow::now_unix();
        let (l, _) = realize("tier1-defect", &defect, true, now);
        assert!(pkg_check::full_pass_owed(&pass_stamps(&l, now), now).is_some());
        assert!(looks(&model, "SessionLook", &defect)["queued"] > 0);
        let legacy = legacy_stamps(&l, now);
        assert!(
            legacy.last_failed(),
            "the old reader read the write as a failed pass"
        );
        assert!(
            pkg_check::full_pass_owed(&legacy, now).is_none(),
            "and held the pass back"
        );
        assert_eq!(looks(&buggy, "SessionLook", &defect)["queued"], 0);
        let _ = std::fs::remove_dir_all(&l.prefix);
        // A writer that records no pass end: its failed pass reads as none.
        let failed = fixed
            .iter()
            .find(|s| s["rec"] == 2 && s["ever_ok"] == 1 && s["running"] == 0)
            .expect("a failed pass after a success is reachable");
        let (l, _) = realize("tier1-unrecorded", failed, false, now);
        assert!(
            !pass_stamps(&l, now).last_failed(),
            "unrecorded, the failure is lost"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }
}

/// Tier-1 fixtures of the derived model `AtpkgFullPassRule` (aterm-spec), shared by this
/// module's conformance test and `cli`'s (a queued pass's stand-down). A model tick is a
/// wall-clock age on each side of the spacing and of the interval.
#[cfg(test)]
pub(crate) mod full_pass_rule_tier1 {
    use super::*;
    use aterm_spec::derive::Model;
    use aterm_update_core::pkg_check::{PassOutcome, Stamps};

    pub(crate) type State = BTreeMap<&'static str, i64>;
    pub(crate) const AGE_SECS: [i64; 5] = [60, 15 * 60, 3 * 3600, 6 * 3600 + 60, 9 * 3600];
    /// What a reader sees of a state: the record and the pass in flight.
    const RECORD: [&str; 6] = [
        "rec",
        "pass_age",
        "ever_ok",
        "ok_age",
        "write_age",
        "running",
    ];

    /// Every state `model` reaches.
    pub(crate) fn reachable(model: &Model) -> Vec<State> {
        let mut order = vec![model.init_state()];
        let mut seen: std::collections::BTreeSet<State> = order.iter().cloned().collect();
        let mut at = 0;
        while at < order.len() {
            for action in model.actions.iter().map(|a| a.name) {
                let mut next = order[at].clone();
                if model.fire(action, &mut next) && seen.insert(next.clone()) {
                    order.push(next);
                }
            }
            at += 1;
        }
        order
    }

    /// One reachable state per distinct record: the readers see nothing else, so states
    /// that differ only in the truth, the queue or the ghosts are read alike.
    pub(crate) fn records(model: &Model) -> Vec<State> {
        let mut seen = std::collections::BTreeSet::new();
        reachable(model)
            .into_iter()
            .filter(|state| seen.insert(RECORD.map(|field| state[field])))
            .collect()
    }

    /// The state `actions` reach from the start, each of them enabled.
    pub(crate) fn walk(model: &Model, actions: &[&str]) -> State {
        let mut state = model.init_state();
        for action in actions {
            assert!(model.fire(action, &mut state), "{action}: {state:?}");
        }
        state
    }

    /// `state` after the look `action` — taken with no child queued, which no reader sees.
    pub(crate) fn looks(model: &Model, action: &str, state: &State) -> State {
        let mut next = State::clone(state);
        next.insert("queued", 0);
        next.insert("behind", 0);
        assert!(model.fire(action, &mut next), "{action}: {state:?}");
        next
    }

    /// `state`'s record laid down at `now` in a fresh store by the real writers: the success
    /// ([`stamp_success`]); with `records_pass_end`, each pass's end ([`stamp_pass_end`]),
    /// else a failed pass as a bare write (the writer before `last_pass`); another writer's
    /// row after the last pass ([`write`], as the vendor head watch writes one); and a live
    /// progress file while a pass is `running`.
    pub(crate) fn realize(
        label: &str,
        state: &State,
        records_pass_end: bool,
        now: i64,
    ) -> (
        crate::testdir::ScratchLayout,
        Option<crate::progress::ProgressSink>,
    ) {
        let layout = crate::testdir::layout(&format!("atpkg-full-pass-{label}-"));
        let at = |ticks: i64| now - AGE_SECS[usize::try_from(ticks).unwrap()];
        let stamp = |unix: i64| aterm_types::rfc3339::format_rfc3339(unix.unsigned_abs());
        let (rec, pass_age) = (state["rec"], state["pass_age"]);
        if state["ever_ok"] == 1 {
            // Strictly before a failed pass that ended in the same saturated age.
            let success = stamp(at(state["ok_age"]) - i64::from(rec == 2));
            stamp_success(&layout, &success).unwrap();
            if records_pass_end {
                stamp_pass_end(&layout, &success, PassOutcome::Ok).unwrap();
            }
        }
        if rec == 2 {
            if records_pass_end {
                stamp_pass_end(&layout, &stamp(at(pass_age)), PassOutcome::Failed).unwrap();
            } else {
                let mut status = seed_for_rewrite(&layout).unwrap();
                status.updated_at = stamp(at(pass_age));
                write(&layout, &status).unwrap();
            }
        }
        let last_write = if rec >= 1 {
            pass_age
        } else if state["ever_ok"] == 1 {
            state["ok_age"]
        } else {
            4
        };
        if state["write_age"] < last_write {
            let mut status = seed_for_rewrite(&layout).unwrap();
            status.programs.insert(
                "claude".into(),
                ProgramStatus {
                    installed_build: Some(1),
                    state: "managed 2.1.280 \u{2014} Anthropic's latest".into(),
                    tree_root: String::new(),
                },
            );
            status.updated_at = stamp(at(state["write_age"]));
            write(&layout, &status).unwrap();
        }
        let sink = (state["running"] >= 1).then(|| {
            crate::progress::ProgressSink::create(&layout.progress_file(), "update").unwrap()
        });
        (layout, sink)
    }

    /// The reader [`pass_stamps`] replaced, kept as the negative control: the attempt is
    /// `updated_at`, else the success; a failure is an attempt after the success, or an
    /// attempt with no success; returned as [`Stamps`] for today's rule to read.
    pub(crate) fn legacy_stamps(layout: &Layout, now: i64) -> Stamps {
        let status = read(layout).unwrap_or_default();
        let unix = |stamp: &str| aterm_update_core::pkg_check::rfc3339_to_unix(stamp.trim());
        let success = unix(&status.last_success_at);
        let attempt = unix(&status.updated_at).or(success);
        let failed = match (attempt, success) {
            (Some(attempt), Some(success)) => attempt > success,
            (Some(_), None) => true,
            (None, _) => false,
        };
        Stamps {
            last_success: success,
            last_attempt: attempt,
            last_outcome: attempt.map(|_| {
                if failed {
                    PassOutcome::Failed
                } else {
                    PassOutcome::Ok
                }
            }),
            in_flight: crate::progress::pass_running(layout, u64::try_from(now).unwrap_or(0)),
            ..Stamps::default()
        }
    }
}
