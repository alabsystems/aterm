// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The operator-readable status record (`<prefix>/status.toml`).

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::Layout;

/// Maximum serialized size of the operator-readable status record.
pub const MAX_STATUS_BYTES: usize = 2 * 1024 * 1024;

/// Maximum number of per-program rows admitted from one status snapshot.
pub const MAX_STATUS_PROGRAMS: usize = 2048;

/// Suffix appended to `outcome` when an update pass used the cached index.
pub const INDEX_FROM_CACHE_CLAUSE: &str = "(index from cache \u{2014} ";

/// One program's last-known state, for `status.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProgramStatus {
    /// The currently-active build, if any.
    pub installed_build: Option<u64>,
    /// The program's state line.
    pub state: String,
    /// Signed `tree_root` (§8) of the active build from the release-key-verified manifest.
    #[serde(default)]
    pub tree_root: String,
}

/// The most refused names one program's entry keeps ([`RefusedShims::names`]).
pub const MAX_REFUSED_RECORDED: usize = 16;

/// The longest refused name an entry keeps, in characters; a longer one is cut, ending in `…`.
pub const MAX_REFUSED_NAME_CHARS: usize = 64;

/// The most builds one program's list keeps an entry for ([`Status::refused_shims`]).
pub const MAX_REFUSED_BUILDS: usize = 8;

/// Exposed names of one build that were refused a shim ([`crate::store::shim_allowed`]).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RefusedShims {
    /// The build whose manifest named them.
    pub build: u64,
    /// Refused names, capped at [`MAX_REFUSED_RECORDED`] entries.
    pub names: Vec<String>,
    /// How many names were refused; more than `names` holds when the list was cut.
    pub count: u64,
}

impl RefusedShims {
    /// The entry for `build` whose landing refused `refused`, or `None` when it refused nothing.
    #[must_use]
    pub fn of(build: u64, refused: &[String]) -> Option<Self> {
        if refused.is_empty() {
            return None;
        }
        let names = refused
            .iter()
            .take(MAX_REFUSED_RECORDED)
            .map(|name| {
                if name.chars().count() <= MAX_REFUSED_NAME_CHARS {
                    return name.clone();
                }
                let mut cut: String = name.chars().take(MAX_REFUSED_NAME_CHARS - 1).collect();
                cut.push('\u{2026}');
                cut
            })
            .collect();
        Some(Self {
            build,
            names,
            count: u64::try_from(refused.len()).unwrap_or(u64::MAX),
        })
    }
}

/// Record that `program`'s `build` was refused a shim for `refused`, pruning removed builds.
/// # Errors
/// [`read_checked`]'s diagnostic for an unreadable record, or the write's.
pub fn record_refused_shims(
    layout: &Layout,
    program: &str,
    build: u64,
    refused: &[String],
) -> io::Result<()> {
    let entry = RefusedShims::of(build, refused);
    let wanted = |have: &BTreeMap<String, Vec<RefusedShims>>| {
        let mut want: BTreeMap<String, Vec<RefusedShims>> = have
            .iter()
            .map(|(p, entries)| {
                let kept: Vec<RefusedShims> = entries
                    .iter()
                    .filter(|r| !(p == program && r.build == build))
                    .filter(|r| layout.build_dir(p, r.build).is_dir())
                    .cloned()
                    .collect();
                (p.clone(), kept)
            })
            .collect();
        if let Some(entry) = &entry {
            want.entry(program.to_string())
                .or_default()
                .push(entry.clone());
        }
        want.retain(|_, kept| !kept.is_empty());
        for kept in want.values_mut() {
            kept.sort_by_key(|r| r.build);
            let over = kept.len().saturating_sub(MAX_REFUSED_BUILDS);
            kept.drain(..over);
        }
        want
    };
    let have = read_checked(layout)?
        .map(|s| s.refused_shims)
        .unwrap_or_default();
    if wanted(&have) == have {
        return Ok(());
    }
    let mut status = seed_for_rewrite(layout)?;
    status.refused_shims = wanted(&status.refused_shims);
    write(layout, &status)
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
    /// The last aggregate decision.
    pub outcome: String,
    /// The rustup toolchain seams atpkg owns, as `rustup:<name>` ([`crate::seam`]).
    #[serde(default)]
    pub seams: Vec<String>,
    /// RFC3339 UTC time of the last full update pass that reached the live signed index.
    #[serde(default)]
    pub last_success_at: String,
    /// The `index_build` the last pass resolved, and when it last changed.
    #[serde(default)]
    pub last_index_build: u64,
    /// See [`Self::last_index_build`].
    #[serde(default)]
    pub index_build_changed_at: String,
    /// How the last full `update` pass ended ([`aterm_update_core::pkg_check::PassOutcome`]).
    #[serde(default)]
    pub last_pass: String,
    /// RFC3339 UTC time that pass ended.
    #[serde(default)]
    pub last_pass_at: String,
    /// The highest index this full pass was asked to check or resolved.
    #[serde(default)]
    pub last_pass_attempted_index_build: u64,
    /// The exact pass-end stamp that owns `last_pass_attempted_index_build`.
    #[serde(default)]
    pub last_pass_attempted_at: String,
    /// Count of full passes that have recorded their end on this store.
    #[serde(default)]
    pub pass_seq: u64,
    /// The [`Self::pass_seq`] of the last full pass that ended `failed` for one reason only.
    #[serde(default)]
    pub stale_index_only_pass_seq: u64,
    /// Per-program states, keyed by program name.
    #[serde(default)]
    pub programs: BTreeMap<String, ProgramStatus>,
    /// Per-program builds (up to [`MAX_REFUSED_BUILDS`]) with refused shim names in `exposes`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub refused_shims: BTreeMap<String, Vec<RefusedShims>>,
    /// Every top-level key this build does not know, carried verbatim through each rewrite.
    #[serde(flatten)]
    pub extra: BTreeMap<String, aterm_toml::Value>,
}

/// Stamp the index build the pass resolved.
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

/// Whether no index update pass has ever completed here.
#[cfg(test)]
#[must_use]
pub(crate) fn never_checked(layout: &Layout) -> bool {
    read(layout).is_none_or(|s| s.last_success_at.trim().is_empty())
}

/// Stamp a full pass that reached the signed index and ran to its end.
/// # Errors
/// [`read_checked`]'s diagnostic for an unreadable record, or the write's.
pub fn stamp_success(layout: &Layout, now: &str) -> io::Result<()> {
    let mut status = seed_for_rewrite(layout)?;
    status.last_success_at = now.to_string();
    status.updated_at = now.to_string();
    write(layout, &status)
}

/// Stamp how a full `update` pass ended.
/// # Errors
/// [`read_checked`]'s diagnostic for an unreadable record, or the write's.
pub fn stamp_pass_end(
    layout: &Layout,
    now: &str,
    outcome: aterm_update_core::pkg_check::PassOutcome,
) -> io::Result<()> {
    stamp_pass_end_with_index(layout, now, outcome, 0)
}

/// [`stamp_pass_end`] with this pass's target or resolved index in the same write.
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

/// [`stamp_pass_end_with_index`], also recording `stale_index_only`.
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

/// Whether `status`'s last full pass ended `failed` only because its index came from cache.
#[inline]
#[must_use]
pub fn stale_index_only_end(status: &Status) -> bool {
    status.pass_seq > 0
        && status.stale_index_only_pass_seq == status.pass_seq
        && status.last_pass == aterm_update_core::pkg_check::PassOutcome::Failed.word()
}

/// Machine-wide stamps on which every lane schedules a full pass, read now.
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

/// How many full passes have recorded their end on this store ([`Status::pass_seq`]).
#[must_use]
pub fn pass_seq(layout: &Layout) -> u64 {
    read(layout).map_or(0, |status| status.pass_seq)
}

impl Status {
    /// The names `program`'s `build` was refused a shim, when its landing recorded any.
    #[inline]
    #[must_use]
    pub fn refused_for(&self, program: &str, build: u64) -> Option<&RefusedShims> {
        self.refused_shims
            .get(program)?
            .iter()
            .find(|r| r.build == build)
    }

    /// Serialize to TOML.
    /// # Errors
    /// The serializer's message, prefixed, when the map cannot be rendered.
    pub fn to_toml(&self) -> Result<String, String> {
        aterm_toml::to_string(self).map_err(|e| format!("serialize status: {e}"))
    }
}

/// Write `status` to `layout.status()`, durably ([`write_durably`]).
pub fn write(layout: &Layout, status: &Status) -> io::Result<()> {
    let text = render(status)?;
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

fn bounded_rows(status: &Status) -> io::Result<()> {
    for (what, rows) in [
        ("programs", status.programs.len()),
        ("refused_shims rows", status.refused_shims.len()),
    ] {
        if rows > MAX_STATUS_PROGRAMS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("status.toml has {rows} {what}; limit is {MAX_STATUS_PROGRAMS}"),
            ));
        }
    }
    if let Some((program, entries)) = status
        .refused_shims
        .iter()
        .find(|(_, entries)| entries.len() > MAX_REFUSED_BUILDS)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "status.toml has {} refused_shims entries for {program}; limit is \
                 {MAX_REFUSED_BUILDS}",
                entries.len()
            ),
        ));
    }
    Ok(())
}

fn render(status: &Status) -> io::Result<String> {
    bounded_rows(status)?;
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

fn write_durably(layout: &Layout, text: &str) -> io::Result<()> {
    static STATUS_TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dest = layout.status();
    let seq = STATUS_TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dest.with_file_name(format!("status.toml.tmp-{}-{seq}", std::process::id()));
    let written = (|| {
        use std::io::Write as _;
        let mut f = crate::platform::open_create_new_write(&tmp, 0o644)?;
        f.write_all(text.as_bytes())?;
        crate::store::sync_contents_or_accept_refusal(&f)?;
        drop(f);
        std::fs::rename(&tmp, &dest)
    })();
    if let Err(error) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(error);
    }
    let _flushed_or_unavailable = aterm_durable::Flush::Durable.dir_best_effort(&layout.prefix)?;
    #[cfg(test)]
    counted::note(layout);
    Ok(())
}

/// How many times each store's record reached the disk in this test process.
#[cfg(test)]
#[path = "status_counted_tests.rs"]
pub(crate) mod counted;

/// Read + parse `status.toml` with explicit admission/parse diagnostics.
pub fn read_checked(layout: &Layout) -> io::Result<Option<Status>> {
    if let Some(held) = held::load(layout) {
        return held;
    }
    read_disk(layout)
}

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
    bounded_rows(&status)?;
    Ok(Some(status))
}

/// Compatibility projection: absent or malformed diagnostics yield `None`.
#[inline]
#[must_use]
pub fn read(layout: &Layout) -> Option<Status> {
    read_checked(layout).ok().flatten()
}

/// Seed a writer that rebuilds the whole record.
/// # Errors
/// The record cannot be read for a reason that is not corruption (a permission, an I/O error, a
/// descriptor limit; it is left as it is), or it is corrupt and could not be kept aside.
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

fn is_corrupt(layout: &Layout, why: &io::Error) -> bool {
    why.kind() == io::ErrorKind::InvalidData
        || std::fs::symlink_metadata(layout.status()).is_ok_and(|m| !m.file_type().is_file())
}

fn heal(layout: &Layout, why: io::Error) -> io::Result<Status> {
    if !is_corrupt(layout, &why) {
        return Err(why);
    }
    let rebuilt = rebuilt_from_store(layout);
    let text = render(&rebuilt)?;
    let path = layout.status();
    let aside = corrupt_path(layout, u64::try_from(crate::flow::now_unix()).unwrap_or(0));
    if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_file()) {
        std::fs::hard_link(&path, &aside)
            .or_else(|_| crate::metadata_io::copy_regular_file(&path, &aside).map(|_| ()))?;
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

fn corrupt_path(layout: &Layout, unix: u64) -> PathBuf {
    let first = layout.prefix.join(format!("status.toml.corrupt-{unix}"));
    if std::fs::symlink_metadata(&first).is_err() {
        return first;
    }
    layout
        .prefix
        .join(format!("status.toml.corrupt-{unix}-{}", std::process::id()))
}

/// The record the store describes when the written one is lost.
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

/// The row the store itself can vouch for, for `program` active at `build`.
#[must_use]
pub fn store_row(layout: &Layout, program: &str, build: u64) -> ProgramStatus {
    let mut tree_root = String::new();
    let state = if crate::alab::build_is_alab(layout, program, build) {
        tree_root = crate::vendor_direct::ProgramStamp::read(layout, program)
            .and_then(|s| s.legacy_root_of(build).map(str::to_string))
            .unwrap_or_default();
        crate::state::alab_kept(build, &crate::alab::build_words(layout, program, build))
    } else if let Some(spec) = crate::vendor_direct::spec(program)
        && let Some(record) =
            crate::vendor_direct::complete_record(&layout.build_dir(program, build))
    {
        tree_root = record.tree_root;
        crate::state::vendor_source(&record.version.to_string(), spec.vendor)
    } else {
        if crate::vendor_direct::is_vendor(program) {
            tree_root = crate::vendor_direct::ProgramStamp::read(layout, program)
                .and_then(|s| s.legacy_root_of(build).map(str::to_string))
                .unwrap_or_default();
        }
        String::from("active")
    };
    ProgramStatus {
        installed_build: Some(build),
        state,
        tree_root,
    }
}

/// `status` as a shell whose `PATH` is `path_var` sees it.
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

/// Hold `layout`'s stamps in memory until the returned guard drops, and write them once then.
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

mod held {
    use super::{Layout, PathBuf, Status, io};

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

    /// Take `status` into the held record: `Some(true)` when it must reach the disk now.
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
#[path = "status_tests.rs"]
mod tests;

/// Tier-1 fixtures of `AtpkgFullPassRule` shared by this module's conformance test and `cli`'s.
#[cfg(test)]
#[path = "status_full_pass_rule_tier1_tests.rs"]
pub(crate) mod full_pass_rule_tier1;
