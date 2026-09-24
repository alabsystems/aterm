// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The operator-readable status record (`…/pkg/status.toml`).
//!
//! A package manager that updates silently (§9) needs a durable observability surface so
//! an operator can answer "is this machine receiving updates, what is installed, and why
//! didn't the last apply happen?" without any prompt. This is that file: the resolved
//! index source, the last aggregate outcome, and a per-program state line (active /
//! tombstoned / deferred / rejected, §5/§7). Best-effort — status is diagnostics, never
//! load-bearing — and durable (Phase 3, 2026-09-22):
//!
//! * written through for what a pass must not lose, coalesced for what it stamps: while a
//!   verb holds the store lock ([`hold`]) a change to a row, the seams or any other field is
//!   on disk at once — a pass killed after activating a build keeps its row and signed root —
//!   while the outcome sentence and the clocks ride in memory and land once, at its end;
//! * written durably: temp + fsync + rename + a sync of the directory, so a power loss
//!   leaves the old record or the new one, never a torn one;
//! * never erased by another build: keys this build does not know ride [`Status::extra`]
//!   through every rewrite, so an older and a newer atpkg sharing a store keep each other's;
//! * healed, never refused: a CORRUPT record (unparsable, not UTF-8, over its bounds, not a
//!   regular file) is kept as `status.toml.corrupt-<unix>` and replaced in one rename by one
//!   rebuilt from the store ([`seed_for_rewrite`]); one that cannot be read for another
//!   reason (a permission, an I/O error) is left alone.

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

/// One program's last-known state, for `status.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProgramStatus {
    /// The currently-active build, if any.
    pub installed_build: Option<u64>,
    /// The program's state line. For a program the index names it is one of the CANONICAL
    /// spellings of [`crate::state`] — `managed <build> — pinned by index <N>` (a
    /// vendor-direct program: `managed <version> — <Vendor> latest`), `system:
    /// <path> — not managed by aterm`, `managed <build> — SHADOWED by <path>`, `extra — not
    /// installed (opt in: …)`, `installed via <protocol>: <path>`, `needs admin — …`,
    /// `unavailable on <target>: <hint>` — the same words the pass log, `doctor` and
    /// `which` print. Faults keep their prefixed free text (`"error: …"`, `"tombstoned:
    /// yanked@N"`, `"deferred: …"`, `"rejected: unsigned index at build N"`, …), which
    /// `doctor` matches by prefix.
    pub state: String,
    /// The SIGNED `tree_root` (§8) of the active build, captured from the release-key-
    /// verified manifest at install/update time. `atpkg verify` recomputes the store tree's
    /// root and compares it to THIS value — a drift audit against the signed root, never a
    /// self-generated hash. Empty ⇒ recorded before verify support / a loose manifest, so
    /// verify reports "cannot attest" (fail-closed, not a pass).
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
    /// The rustup toolchain seams atpkg owns, as `rustup:<name>` ([`crate::seam`],
    /// Lockstep S1): written when an attach succeeds, dropped by `seam detach` and
    /// `uninstall --all`, and what every re-assertion walks. Absent from records
    /// written before seams existed, so it defaults EMPTY — and an older app reading
    /// a newer record simply ignores the key.
    #[serde(default)]
    pub seams: Vec<String>,
    /// RFC3339 UTC time of the last SUCCESSFUL update pass — stamped by
    /// [`stamp_success`] alone, never by the per-program writers, which move
    /// `updated_at` on every write (a failed resolve, a shadow reconcile, a seed offer
    /// row) and so made "N day(s) since the last successful update" a sentence about
    /// the last write of any kind (2026-09-10 audit). Empty ⇒ no successful pass has
    /// completed since this field existed: [`never_checked`] reads it, and `aterm pkg
    /// doctor` reports it (no verb prints it on stderr since Phase 2, 2026-09-22). A
    /// record written before the field parses as empty — a machine that has been
    /// updating for a month reads "never checked" for exactly one pass, then the next
    /// success stamps it.
    #[serde(default)]
    pub last_success_at: String,
    /// RFC3339 UTC time the signed index LISTING was last REACHED over the network —
    /// as against served from the §14 cache after a rate limit, an outage or a proxy
    /// refused it. Stamped by [`stamp_index_freshness`] from what the resolve measured
    /// ([`crate::flow::last_resolve`]); a pass that ran on the cache leaves it where
    /// it was. `last_success_at` could not say this: a cached resolve is a pass that
    /// "succeeded", and every surface stayed green while the managed `claude` could
    /// freeze at an old pin (audit 2026-09-14). Empty before 2026-09-15.
    #[serde(default)]
    pub last_index_reached_at: String,
    /// The `index_build` the last pass resolved, and when it last CHANGED — the
    /// publisher's own pulse. `doctor`'s "publishing looks frozen" used to read a clock
    /// that advances on every no-op pass, so a dead vendor lane, a stuck lock or an
    /// expired token upstream was undetectable from a client (audit 2026-09-14).
    #[serde(default)]
    pub last_index_build: u64,
    /// See [`Self::last_index_build`]. Empty before 2026-09-15.
    #[serde(default)]
    pub index_build_changed_at: String,
    /// How the last full `update` pass ended — `ok`, `offline` or `failed`
    /// ([`aterm_update_core::pkg_check::PassOutcome`]) — stamped by [`stamp_pass_end`]
    /// alone. What the schedulers read of a pass: `updated_at` moves on every write, and
    /// read as a pass's end it turned a vendor head-watch write into a failed pass. Empty
    /// before 2026-09-23.
    #[serde(default)]
    pub last_pass: String,
    /// RFC3339 UTC time that pass ended. See [`Self::last_pass`].
    #[serde(default)]
    pub last_pass_at: String,
    /// The highest index this full pass was asked to check or actually resolved,
    /// provided it attempted a signed index resolve. Zero means unknown
    /// (including records from older atpkg builds). Written
    /// together with `last_pass` and `last_pass_at`, so a scheduler can distinguish
    /// a genuinely newer publication from another lane's attempt at the SAME
    /// index even if that attempt failed before it resolved the listing.
    #[serde(default)]
    pub last_pass_attempted_index_build: u64,
    /// The exact pass-end stamp that owns `last_pass_attempted_index_build`.
    /// A previous atpkg binary preserves unknown fields while rewriting the
    /// known pass end; comparing this stamp avoids borrowing its old target.
    #[serde(default)]
    pub last_pass_attempted_at: String,
    /// RFC3339 UTC time before which the metered GitHub API refuses this machine: the reset
    /// a rate-limited index listing named (`retry-after`, or `x-ratelimit-reset` with the
    /// window spent). The schedulers hold the next full pass until it; empty when none.
    #[serde(default)]
    pub metered_hold_until: String,
    /// Per-program states, keyed by program name.
    #[serde(default)]
    pub programs: BTreeMap<String, ProgramStatus>,
    /// Every top-level key this build does not know, carried verbatim through each
    /// rewrite: an older build erased the freshness fields a newer one wrote every time
    /// it rebuilt the record (the owner's Mac, every six hours), so no build drops a key.
    #[serde(flatten)]
    pub extra: BTreeMap<String, aterm_toml::Value>,
}

/// Stamp what the pass's index resolve MEASURED: `last_index_reached_at = now` when the
/// listing answered over the network (`reached`), and `last_index_build` /
/// `index_build_changed_at` when `index_build` differs from the one recorded (a `None`
/// build — a pass with no index in hand — changes neither). Best-effort like every
/// status write; `updated_at` moves with it. Seeded through [`seed_for_rewrite`]: a corrupt
/// record is kept aside and rebuilt, one unreadable for another reason is left alone.
///
/// # Errors
/// [`read_checked`]'s diagnostic for an unreadable record, or the write's.
pub fn stamp_index_freshness(
    layout: &Layout,
    now: &str,
    reached: bool,
    index_build: Option<u64>,
) -> io::Result<()> {
    let mut status = seed_for_rewrite(layout)?;
    if reached {
        status.last_index_reached_at = now.to_string();
    }
    if let Some(build) = index_build
        && build != status.last_index_build
    {
        status.last_index_build = build;
        status.index_build_changed_at = now.to_string();
    }
    status.updated_at = now.to_string();
    write(layout, &status)
}

/// Whether NO index update pass has ever completed successfully on this machine:
/// `status.toml` absent, unreadable, or present with an empty
/// [`Status::last_success_at`]. This is the R3 condition, and nothing weaker: a record
/// that exists because a failed resolve wrote its `*index*` row is not a check that
/// ran. DATA, never a line on a shell (Phase 2, 2026-09-22): `aterm pkg doctor` reports
/// it, and says what is true meanwhile — the vendor programs update without it.
#[must_use]
pub fn never_checked(layout: &Layout) -> bool {
    read(layout).is_none_or(|s| s.last_success_at.trim().is_empty())
}

/// Stamp `last_success_at = now` (and `updated_at`, which every write moves) on the
/// record, creating a minimal one when none exists. Called by the CLI at the END of a
/// pass that resolved the signed index and ran to its end — the one event that makes
/// "packages can be updated on this machine" true. A member that FAILED inside such a
/// pass is recorded in its own row, not here: until 2026-09-14 only a zero-failure pass
/// stamped this, so one refused member kept every verb saying no check had ever run.
/// Best-effort like every status write, and seeded through [`seed_for_rewrite`]: a corrupt
/// record is kept aside and rebuilt, one unreadable for another reason is left alone.
///
/// # Errors
/// [`read_checked`]'s diagnostic for an unreadable record, or the write's.
pub fn stamp_success(layout: &Layout, now: &str) -> io::Result<()> {
    let mut status = seed_for_rewrite(layout)?;
    status.last_success_at = now.to_string();
    status.updated_at = now.to_string();
    write(layout, &status)
}

/// What a full pass leaves of the metered hold ([`stamp_pass_end`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeteredHold {
    /// Its listing was refused until this unix second: hold the next full pass until then.
    Until(i64),
    /// Its listing answered: whatever hold stood is over.
    Lifted,
    /// It learned nothing about the budget (offline, no listing asked): leave it.
    Kept,
}

/// Stamp how a full `update` pass ended — `last_pass` (the outcome's word) and
/// `last_pass_at = now` — and what it learned of the metered hold, with `updated_at` as
/// every write moves it. The ONE writer of those keys, called once at the pass's end
/// (`cli`'s recorded pass); the schedulers read them through
/// [`aterm_update_core::pkg_check::Stamps`]. Seeded through [`seed_for_rewrite`] like
/// every stamp writer.
///
/// # Errors
/// [`read_checked`]'s diagnostic for an unreadable record, or the write's.
pub fn stamp_pass_end(
    layout: &Layout,
    now: &str,
    outcome: aterm_update_core::pkg_check::PassOutcome,
    hold: MeteredHold,
) -> io::Result<()> {
    stamp_pass_end_with_index(layout, now, outcome, hold, 0)
}

/// [`stamp_pass_end`] with this pass's target or resolved index, written in
/// the SAME durable status update as the outcome and end time. An unknown
/// target clears the previous pass's witness rather than borrowing it.
///
/// # Errors
/// [`read_checked`]'s diagnostic for an unreadable record, or the write's.
pub fn stamp_pass_end_with_index(
    layout: &Layout,
    now: &str,
    outcome: aterm_update_core::pkg_check::PassOutcome,
    hold: MeteredHold,
    attempted_index_build: u64,
) -> io::Result<()> {
    let mut status = seed_for_rewrite(layout)?;
    status.last_pass = outcome.word().to_string();
    // Older binaries write whole-second `last_pass_at` and preserve unknown
    // fields via `extra`. The fractional-zero suffix marks this writer, so
    // even an old pass ending in the SAME second cannot inherit this target.
    // The suffix is a format marker, not a claim of subsecond clock precision.
    let witness_at = (attempted_index_build > 0)
        .then(|| now.strip_suffix('Z'))
        .flatten()
        .filter(|stamp| !stamp.contains('.'))
        .map(|stamp| format!("{stamp}.000000000Z"));
    status.last_pass_at = witness_at.clone().unwrap_or_else(|| now.to_string());
    status.last_pass_attempted_index_build = attempted_index_build;
    status.last_pass_attempted_at = witness_at.unwrap_or_default();
    match hold {
        MeteredHold::Until(until) => {
            status.metered_hold_until =
                aterm_types::rfc3339::format_rfc3339(u64::try_from(until).unwrap_or(0));
        }
        MeteredHold::Lifted => status.metered_hold_until.clear(),
        MeteredHold::Kept => {}
    }
    status.updated_at = now.to_string();
    write(layout, &status)
}

/// THE MACHINE-WIDE STAMPS every lane schedules a full pass on — the window's gate and walk,
/// the session lane, a window's launch — read NOW: `status.toml`'s
/// ([`aterm_update_core::pkg_check::Stamps::read`]), with the index build raised to the one
/// VERIFIED here ([`crate::index_probe::verified_floor`], so an index a sibling landed counts
/// where the record did not move) and whether a pass is installing now
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

impl Status {
    /// Serialize to TOML.
    ///
    /// # Errors
    /// The serializer's message, prefixed, when the map cannot be rendered.
    pub fn to_toml(&self) -> Result<String, String> {
        aterm_toml::to_string(self).map_err(|e| {
            // Manual concat of the previous `format!("serialize status: {e}")` —
            // byte-identical (`{e}` is `Display`, which is what `to_string`
            // renders): the `format!` expansion embeds `fmt::Arguments`
            // construction (with inlined `unsafe`) that the strict Trust gate
            // cannot lower and fails closed on.
            let mut m = String::from("serialize status: ");
            m.push_str(&e.to_string());
            m
        })
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
    // The record as it stood, for the package log's transitions (Phase 4): the held record
    // while a verb holds the lock (memory), else the file. Every row writer of every lane
    // comes through here, which is what makes this the one place a transition is seen.
    // A record that cannot be read is no baseline: nothing is logged against it.
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

/// Whether `a` and `b` differ only in what a pass stamps as it goes — the outcome sentence
/// and the clocks. That is all a hold coalesces: a lost stamp is re-stamped by the next
/// pass, while a lost row (an activated build and its signed root, a seam, a tombstone)
/// made the next pass record a new build under an old root, which `verify` calls drift.
fn only_stamps_differ(a: &Status, b: &Status) -> bool {
    let unstamped = |s: &Status| Status {
        updated_at: String::new(),
        outcome: String::new(),
        last_success_at: String::new(),
        last_index_reached_at: String::new(),
        last_index_build: 0,
        index_build_changed_at: String::new(),
        last_pass: String::new(),
        last_pass_at: String::new(),
        last_pass_attempted_index_build: 0,
        last_pass_attempted_at: String::new(),
        metered_hold_until: String::new(),
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
/// temp beside it, `fsync`, `rename(2)`, then a sync of the directory that makes the rename
/// durable. The temp is reclaimed on every failure arm — its name carries this pid and
/// nothing sweeps `status.toml.tmp-*`, so a full disk would strand one per pass.
fn write_durably(layout: &Layout, text: &str) -> io::Result<()> {
    let dest = layout.status();
    // Manual rendering of `format!("status.toml.tmp-{}", pid)`: the `format!` expansion
    // embeds `fmt::Arguments` construction the strict Trust gate cannot lower.
    let mut tmp_name = String::from("status.toml.tmp-");
    tmp_name.push_str(&crate::dec_u64(u64::from(std::process::id())));
    let tmp = dest.with_file_name(tmp_name);
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

/// Seed a writer that REBUILDS the whole record: the record on disk, a minimal schema-1
/// one when this store has none yet, or — when the record is CORRUPT — one rebuilt from
/// the store, the corrupt bytes kept aside ([`heal`]).
///
/// A corrupt record used to be refused by every writer, forever: `last_success_at` could
/// never be stamped again, the window's launch pass was due at every launch and "never
/// checked" stood until a person moved the file (2026-09-22). It is healed instead, and
/// the bytes an operator might still salvage are kept as `status.toml.corrupt-<unix>`
/// rather than overwritten.
///
/// # Errors
/// The record cannot be read for a reason that is not corruption (a permission, an I/O
/// error, a descriptor limit) — it is left exactly as it is — or it is corrupt and could
/// not be kept aside.
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

/// Replace a corrupt record with the one the store describes ([`rebuilt_from_store`]),
/// keeping the corrupt one as `status.toml.corrupt-<unix>` (the prefix root, beside it),
/// said once as an unasked notice. The path is never left empty for a regular file: its
/// bytes are linked (or copied) aside and the rebuilt record renamed over it, so a kill in
/// between leaves the old bytes or the new record, and a reader never reads "no record".
/// `Err` — nothing replaced — when `why` is not corruption ([`is_corrupt`]) or the bytes
/// could not be kept.
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
    let mut name = String::from("status.toml.corrupt-");
    name.push_str(&crate::dec_u64(unix));
    let first = layout.prefix.join(&name);
    if std::fs::symlink_metadata(&first).is_err() {
        return first;
    }
    name.push('-');
    name.push_str(&crate::dec_u64(u64::from(std::process::id())));
    layout.prefix.join(name)
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
/// by its recorded version, an index build as `active` (the legacy word every reader
/// accepts and the next pass rewrites to its pin), and NO signed root — the store holds no
/// signature, so the pass's root recovery re-derives it.
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

/// `status` as a shell whose `PATH` is `path_var` sees it: every managed row of an active
/// program one of whose tools a foreign executable out-ranks on that `PATH` reads
/// `managed <build> — SHADOWED by <path>`. SHADOWED is per-shell truth, so the passes no
/// longer record it (Phase 3); the surfaces that answer for a shell compute it when they
/// read — `doctor` and `which` against their own `PATH`, the Settings page against the
/// login shell's. An agent program whose `agents/` twin is current is never shadowed here
/// (every aterm tab puts `agents/` first), a dev-linked program never, and a row an older
/// client recorded as SHADOWED is left as written — the next pass lifts it.
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

/// Hold `layout`'s STAMPS in memory until the returned guard drops, and write them ONCE
/// then — how a mutating verb writes `status.toml` once however many times it moves the
/// outcome and the clocks (it used to rewrite the whole file three to N times per pass).
/// Every other change is written through at once ([`write`]): atpkg has no signal
/// handler, so a pass killed by a Ctrl-C, a logout or a SIGKILL must already have put its
/// rows on disk. Nests: an inner hold of the same store is part of the outer one. The CLI
/// edge takes it right after the store lock, so the record is on disk before the lock is
/// released.
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

    fn layout(label: &str) -> Layout {
        let p = std::env::temp_dir().join(format!("atpkg-status-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).unwrap();
        Layout { prefix: p }
    }

    /// The freshness stamps (2026-09-15): the reach time moves only when the listing
    /// answered; the build-changed time moves only when the build differs from the one
    /// recorded; a pass with no index in hand moves neither.
    #[test]
    fn index_freshness_stamps_only_what_the_resolve_measured() {
        let dir = std::env::temp_dir().join(format!("atpkg-status-fresh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let layout = Layout {
            prefix: dir.clone(),
        };
        stamp_index_freshness(&layout, "2026-09-15T10:00:00Z", true, Some(32)).unwrap();
        let s = read(&layout).unwrap();
        assert_eq!(s.last_index_reached_at, "2026-09-15T10:00:00Z");
        assert_eq!(s.last_index_build, 32);
        assert_eq!(s.index_build_changed_at, "2026-09-15T10:00:00Z");
        // A cached pass: the reach time stays, the build is the same, nothing moves.
        stamp_index_freshness(&layout, "2026-09-16T10:00:00Z", false, Some(32)).unwrap();
        let s = read(&layout).unwrap();
        assert_eq!(s.last_index_reached_at, "2026-09-15T10:00:00Z");
        assert_eq!(s.index_build_changed_at, "2026-09-15T10:00:00Z");
        assert_eq!(s.updated_at, "2026-09-16T10:00:00Z");
        // A reached pass on a newer build: both move.
        stamp_index_freshness(&layout, "2026-09-17T10:00:00Z", true, Some(33)).unwrap();
        let s = read(&layout).unwrap();
        assert_eq!(s.last_index_reached_at, "2026-09-17T10:00:00Z");
        assert_eq!(s.last_index_build, 33);
        assert_eq!(s.index_build_changed_at, "2026-09-17T10:00:00Z");
        // No index in hand: the build fields are untouched.
        stamp_index_freshness(&layout, "2026-09-18T10:00:00Z", true, None).unwrap();
        let s = read(&layout).unwrap();
        assert_eq!(s.last_index_build, 33);
        assert_eq!(s.index_build_changed_at, "2026-09-17T10:00:00Z");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pass_end_binds_its_index_target_and_clears_unknown_successors() {
        use aterm_update_core::pkg_check::{PassOutcome, Stamps};
        let l = layout("pass-target");
        stamp_index_freshness(&l, "2026-09-24T12:00:00Z", true, Some(44)).unwrap();
        stamp_pass_end_with_index(
            &l,
            "2026-09-24T12:01:00Z",
            PassOutcome::Ok,
            MeteredHold::Lifted,
            45,
        )
        .unwrap();
        let status = read(&l).unwrap();
        assert_eq!(status.last_index_build, 44);
        assert_eq!(status.last_pass_attempted_index_build, 45);
        assert_eq!(status.last_pass_at, status.last_pass_attempted_at);
        assert!(status.last_pass_at.ends_with(".000000000Z"));
        let stamps = Stamps::read(&l.status());
        assert_eq!(stamps.last_pass_target.map(|(_, build)| build), Some(45));
        let now = aterm_update_core::pkg_check::rfc3339_to_unix("2026-09-24T12:02:00Z").unwrap();
        assert!(!stamps.completed_older_index_pass(45, now));
        assert!(stamps.completed_older_index_pass(46, now));

        // An older atpkg binary knows `last_pass_at`, but carries both new
        // fields through its flattened extra map unchanged. Even an old pass
        // ending in the SAME second must not inherit this target.
        let mut old_writer = status;
        old_writer.last_pass = "failed".to_string();
        old_writer.last_pass_at = "2026-09-24T12:01:00Z".to_string();
        write(&l, &old_writer).unwrap();
        let stale = Stamps::read(&l.status());
        assert_eq!(stale.last_pass_target, None);
        assert!(!stale.completed_older_index_pass(46, now));

        stamp_pass_end(
            &l,
            "2026-09-24T12:02:00Z",
            PassOutcome::Failed,
            MeteredHold::Kept,
        )
        .unwrap();
        let next = Stamps::read(&l.status());
        assert_eq!(read(&l).unwrap().last_pass_attempted_index_build, 0);
        assert_eq!(
            next.last_pass_target, None,
            "the old target is not borrowed"
        );
        assert!(!next.completed_older_index_pass(46, now));
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn write_then_read_round_trips() {
        let l = layout("rt");
        let mut programs = BTreeMap::new();
        programs.insert(
            "ay".to_string(),
            ProgramStatus {
                installed_build: Some(18),
                state: "active".into(),
                tree_root: "abc123".into(),
            },
        );
        programs.insert(
            "trust".to_string(),
            ProgramStatus {
                installed_build: None,
                state: "tombstoned: yanked@4790".into(),
                tree_root: String::new(),
            },
        );
        let s = Status {
            schema: 1,
            updated_at: "2026-06-29T00:00:00Z".into(),
            enabled: true,
            index_source: "alabsystems/aterm-toolchain-index".into(),
            outcome: "up to date".into(),
            seams: Vec::new(),
            last_success_at: "2026-06-29T00:00:00Z".into(),
            last_index_reached_at: String::new(),
            last_index_build: 0,
            index_build_changed_at: String::new(),
            last_pass: String::new(),
            last_pass_at: String::new(),
            last_pass_attempted_index_build: 0,
            last_pass_attempted_at: String::new(),
            metered_hold_until: String::new(),
            programs,
            extra: Default::default(),
        };
        write(&l, &s).unwrap();
        let back = read(&l).expect("status reads back");
        assert_eq!(back, s);
        // The signed tree_root survives the TOML round-trip (drives `atpkg verify`).
        assert_eq!(back.programs["ay"].tree_root, "abc123");
        // It is valid TOML on disk.
        let text = std::fs::read_to_string(l.status()).unwrap();
        let _: aterm_toml::Value = aterm_toml::from_str(&text).expect("valid TOML");
        assert!(text.contains("index_source = \"alabsystems/aterm-toolchain-index\""));
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn write_is_atomic_no_temp_left_behind() {
        let l = layout("atomic");
        write(
            &l,
            &Status {
                schema: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(&l.prefix)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "no temp file should remain after rename"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// The same promise on the FAILING path (2026-09-16). The temp name carries this
    /// process's pid, so a write or rename that fails stranded `status.toml.tmp-<pid>`
    /// in the prefix root — a name nothing has ever swept: neither `gc` nor `doctor`
    /// scans for it, and the next pass runs under a new pid, so the leftovers do not
    /// even overwrite each other. A machine whose disk stays full writes status
    /// several times per pass and left one more file behind on every pass,
    /// unreclaimable, for as long as the disk stayed full. Staged with the one failure
    /// a test can force deterministically: a `status.toml` that is a DIRECTORY, so the
    /// bytes reach the temp and the rename is what refuses.
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
        // EISDIR — 21 on every unix this ships to. Pinning it keeps the test honest:
        // the bytes DID reach the temp and the rename is what refused, which is
        // precisely the state that used to strand the temp. An error raised before the
        // temp was ever created would prove nothing.
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

    /// A writer that REBUILDS the record answers one it could not READ by keeping the
    /// bytes aside and rebuilding from the store (Phase 3): a truncated file (the shape a
    /// power loss leaves) and garbage both heal, the next stamp lands, and nothing an
    /// operator could salvage is overwritten. Absent is not unreadable: a virgin store
    /// seeds a fresh record and keeps nothing aside.
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
        stamp_index_freshness(&l, "2026-09-16T00:00:00Z", true, Some(7)).unwrap();
        assert_eq!(read(&l).unwrap().last_index_build, 7);
        // HEALED IN PLACE, even inside a hold: the rebuilt record is on disk the moment the
        // corrupt one is kept aside — never "no record" for the length of a pass, which a
        // kill then left for the next writer to take as a virgin store.
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

    /// A READ THAT FAILED IS NOT A CORRUPT RECORD: a permission (or an I/O error, a
    /// descriptor limit) leaves a healthy record exactly where it is — never moved aside,
    /// never replaced by a rebuilt one missing its stamps and signed roots.
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

    /// THE KEYS ANOTHER BUILD WROTE SURVIVE EVERY REWRITE: a newer atpkg's field (and a
    /// table) rides through an older writer's full rebuild — the owner's Mac had an older
    /// build erasing the freshness fields every six hours.
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

    /// ONE WRITE PER VERB FOR THE STAMPS: while a store is held, the outcome and the clocks
    /// land in memory and a reader in this process sees them; the file is written once,
    /// when the hold ends, and a hold nothing wrote into writes nothing.
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
            stamp_index_freshness(&l, "2026-09-17T00:00:00Z", true, Some(9)).unwrap();
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

    /// A ROW IS WRITTEN THROUGH, EVEN HELD: atpkg has no signal handler, so a pass killed
    /// after it activated a build must already have that build's row and signed root on
    /// disk — a lost row made the next pass record the new build under the old root, which
    /// `verify` reports as drift. Only the stamps that follow it wait for the end.
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

    /// Lockstep S1: the rustup `seams` record round-trips beside the program rows —
    /// aterm-toml orders values before sub-tables, so the array never lands inside
    /// the last `[programs.*]` table — and a record that predates the field (the
    /// 0.65 shape) parses as "no seams", never as an error.
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
            last_index_reached_at: String::new(),
            last_index_build: 0,
            index_build_changed_at: String::new(),
            last_pass: String::new(),
            last_pass_at: String::new(),
            last_pass_attempted_index_build: 0,
            last_pass_attempted_at: String::new(),
            metered_hold_until: String::new(),
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

        // The 0.65 shape: no `seams` key at all.
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

    /// R3: "never checked" is exactly `status.toml` absent OR `last_success_at` empty —
    /// a record a FAILED pass wrote (its `*index*` row, a fresh `updated_at`) still reads
    /// as never checked; only [`stamp_success`] clears it, and the stamp round-trips
    /// beside the program rows.
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

    /// NO SURFACE CLAIMS THE UNTRUE HALF (Phase 2, 2026-09-22; until then this test pinned
    /// the stderr line every console edge printed): "packages cannot be updated until the
    /// first pass completes" was false once the vendor programs stopped waiting on the
    /// index (design §1), and the line itself is gone from every edge — the read-only
    /// verbs, the session launch, the window. Scanned over this crate's sources, the three
    /// edges that printed it and the manual; the needle is spelled in pieces so this test
    /// is not its own hit.
    #[test]
    fn no_surface_claims_packages_cannot_update_before_the_first_pass() {
        let needles = [["cannot be updated until", " the first pass"].concat()];
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files: Vec<std::path::PathBuf> = Vec::new();
        let mut dirs = vec![root.join("src")];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    dirs.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    files.push(path);
                }
            }
        }
        for edge in [
            "../aterm/src/main.rs",
            "../aterm-gui/src/lib.rs",
            "../aterm-update-core/src/pkg_check.rs",
            "../aterm-cli/src/manual.rs",
        ] {
            files.push(root.join(edge));
        }
        assert!(files.len() > 40, "{}", files.len());
        for file in &files {
            let text = std::fs::read_to_string(file).unwrap();
            for needle in &needles {
                assert!(
                    !text.contains(needle.as_str()),
                    "{}: {needle}",
                    file.display()
                );
            }
        }
    }

    /// TIER-1 OF THE DERIVED MODEL `AtpkgFullPassRule` (aterm-spec): every record the model
    /// reaches at `Buggy=0` is laid down in a real store by the real writers
    /// ([`full_pass_rule_tier1::realize`]) and read back as the session lane reads it
    /// ([`pass_stamps`], then [`aterm_update_core::pkg_check::full_pass_owed`]): the failure
    /// it reads is the model's `verdict`, the hold and the pass in flight are the record's,
    /// and the lane starts a pass exactly when `SessionLook` does.
    ///
    /// THE HISTORICAL DEFECT, REPLAYED ON REAL TEXT (the negative control): the reader this
    /// replaced ([`full_pass_rule_tier1::legacy_stamps`], ac5b4c144's order of `updated_at`
    /// and the success) over every record `Buggy=1` reaches, laid down by the writer of that
    /// day, is `Buggy=1`'s lane — and on the replayed defect (a success an interval old,
    /// then a head-watch write) it holds back the pass the real reader owes. THE UPGRADE: the
    /// current reader over those same pre-`last_pass` records reads no failure — the record
    /// reads by its success alone — so a fallback to `updated_at` is caught here. And a
    /// WRITER that records no pass end reads a failed pass as none.
    #[test]
    fn the_full_pass_rule_conforms_to_its_derived_model() {
        use super::full_pass_rule_tier1::{legacy_stamps, looks, realize, records};
        use aterm_spec::derive::atpkg_full_pass_rule_model;
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
            assert_eq!(
                stamps.metered_hold_in(now) > 0,
                state["hold"] > 0,
                "{state:?}"
            );
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

/// THE TIER-1 FIXTURES OF THE DERIVED MODEL `AtpkgFullPassRule` (aterm-spec), shared by
/// this module's conformance (the writers and the session lane) and `cli`'s (a queued
/// pass's stand-down). A model tick is a wall-clock age on each side of the spacing and of
/// the interval; a hold tick, minutes inside GitHub's hour.
#[cfg(test)]
pub(crate) mod full_pass_rule_tier1 {
    use super::*;
    use aterm_spec::derive::Model;
    use aterm_update_core::pkg_check::{PassOutcome, Stamps};

    pub(crate) type State = BTreeMap<&'static str, i64>;
    pub(crate) const AGE_SECS: [i64; 5] = [60, 15 * 60, 3 * 3600, 6 * 3600 + 60, 9 * 3600];
    const HOLD_SECS: [i64; 3] = [0, 10 * 60, 30 * 60];
    /// What a reader sees of a state: the record and the pass in flight.
    const RECORD: [&str; 7] = [
        "rec",
        "pass_age",
        "ever_ok",
        "ok_age",
        "write_age",
        "hold",
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
    /// ([`stamp_success`]); with `records_pass_end`, each pass's end ([`stamp_pass_end`]: the
    /// success's own, `ok`, then a failed one, with the hold a rate limit named) — without
    /// it the writer of before `last_pass`, whose failed pass left a bare write; another
    /// writer's row after the last pass ([`write`], as the vendor head watch writes one);
    /// and a live progress file while a pass is `running`.
    pub(crate) fn realize(
        label: &str,
        state: &State,
        records_pass_end: bool,
        now: i64,
    ) -> (Layout, Option<crate::progress::ProgressSink>) {
        let p =
            std::env::temp_dir().join(format!("atpkg-full-pass-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        let layout = Layout { prefix: p };
        let at = |ticks: i64| now - AGE_SECS[usize::try_from(ticks).unwrap()];
        let stamp = |unix: i64| aterm_types::rfc3339::format_rfc3339(unix.unsigned_abs());
        let hold = |fallback: MeteredHold| match state["hold"] {
            0 => fallback,
            left => MeteredHold::Until(now + HOLD_SECS[usize::try_from(left).unwrap()]),
        };
        let (rec, pass_age) = (state["rec"], state["pass_age"]);
        if state["ever_ok"] == 1 {
            // Strictly before a failed pass that ended in the same saturated age.
            let success = stamp(at(state["ok_age"]) - i64::from(rec == 2));
            stamp_success(&layout, &success).unwrap();
            if records_pass_end {
                let own = if rec == 1 {
                    hold(MeteredHold::Lifted)
                } else {
                    MeteredHold::Lifted
                };
                stamp_pass_end(&layout, &success, PassOutcome::Ok, own).unwrap();
            }
        }
        if rec == 2 {
            if records_pass_end {
                stamp_pass_end(
                    &layout,
                    &stamp(at(pass_age)),
                    PassOutcome::Failed,
                    hold(MeteredHold::Kept),
                )
                .unwrap();
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

    /// THE READER THIS REPLACED, kept as the negative control (ac5b4c144's
    /// `pkg_check::last_attempt_at` and `Stamps::last_failed`): the attempt is `updated_at`,
    /// else the success; a failure is an attempt after the success, or any attempt and none
    /// — as the [`Stamps`] today's rule reads, the spacing counted from that attempt.
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
