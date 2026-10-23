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

/// How many times each store's record reached the disk in this test process.
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
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn layout(label: &str) -> crate::testdir::ScratchLayout {
        crate::testdir::layout(&format!("atpkg-status-{label}-"))
    }

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
        stamp_pass_end(&layout, "2026-09-16T10:00:00Z", PassOutcome::Failed).unwrap();
        stamp_index_build(&layout, "2026-09-16T10:00:00Z", Some(32)).unwrap();
        let s = read(&layout).unwrap();
        assert_eq!(s.last_success_at, "2026-09-15T10:00:00Z");
        assert_eq!(s.last_pass, "failed");
        assert_eq!(s.last_pass_at, "2026-09-16T10:00:00Z");
        assert_eq!(s.index_build_changed_at, "2026-09-15T10:00:00Z");
        assert_eq!(s.updated_at, "2026-09-16T10:00:00Z");
        assert!(Stamps::read(&layout.status()).last_failed());
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
        stamp_index_build(&l, "2026-09-16T00:00:00Z", Some(7)).unwrap();
        assert_eq!(read(&l).unwrap().last_index_build, 7);
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
            let _ = std::fs::remove_dir_all(&l.prefix);
            aterm_uds::host::could_not_run(
                "a mode-0 file read anyway: root, or a file system that ignores modes",
            );
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
    /// build must leave its row and signed root on disk.
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

        std::fs::write(l.status(), status.to_toml().unwrap()).unwrap();
        let read_error = read_checked(&l).unwrap_err();
        assert_eq!(read_error.kind(), io::ErrorKind::InvalidData);
        assert!(read_error.to_string().contains("limit is"));
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

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
            refused_shims: BTreeMap::new(),
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

    #[test]
    fn refused_shims_are_recorded_per_build_bounded_and_dropped_with_the_build() {
        let l = layout("refused-shims");
        let names = |list: &[&str]| list.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        std::fs::create_dir_all(l.build_dir("ay", 18)).unwrap();
        std::fs::create_dir_all(l.build_dir("ay", 19)).unwrap();
        std::fs::create_dir_all(l.build_dir("ny", 4)).unwrap();

        record_refused_shims(&l, "ay", 18, &[]).unwrap();
        assert!(
            !l.status().exists(),
            "a landing that refused nothing writes nothing"
        );

        record_refused_shims(&l, "ay", 18, &names(&["pwsh", "git"])).unwrap();
        let long = "x".repeat(MAX_REFUSED_NAME_CHARS + 9);
        let many: Vec<String> = std::iter::once(long.clone())
            .chain((0..MAX_REFUSED_RECORDED + 3).map(|i| format!("a/{i}")))
            .collect();
        record_refused_shims(&l, "ny", 4, &many).unwrap();
        record_refused_shims(&l, "ay", 19, &names(&["zsh"])).unwrap();
        let back = read_checked(&l).unwrap().expect("recorded");
        assert_eq!(
            back.refused_for("ay", 18),
            Some(&RefusedShims {
                build: 18,
                names: names(&["pwsh", "git"]),
                count: 2,
            })
        );
        assert_eq!(
            back.refused_for("ay", 19).map(|r| r.names.clone()),
            Some(names(&["zsh"]))
        );
        assert_eq!(back.refused_for("ay", 17), None);
        let ny = back.refused_for("ny", 4).expect("ny's entry");
        assert_eq!(ny.names.len(), MAX_REFUSED_RECORDED);
        assert_eq!(ny.count, u64::try_from(many.len()).unwrap());
        assert_eq!(ny.names[0].chars().count(), MAX_REFUSED_NAME_CHARS);
        assert!(ny.names[0].ends_with('\u{2026}') && long.starts_with(&ny.names[0][..60]));

        #[cfg(unix)]
        {
            let writes = counted::writes(&l);
            record_refused_shims(&l, "ay", 18, &names(&["pwsh", "git"])).unwrap();
            assert_eq!(
                counted::writes(&l),
                writes,
                "an unchanged entry is not rewritten"
            );
        }

        for build in 20..20 + u64::try_from(MAX_REFUSED_BUILDS).unwrap() {
            std::fs::create_dir_all(l.build_dir("ay", build)).unwrap();
            record_refused_shims(&l, "ay", build, &names(&["fish"])).unwrap();
        }
        let back = read_checked(&l).unwrap().expect("recorded");
        let builds: Vec<u64> = back.refused_shims["ay"].iter().map(|r| r.build).collect();
        assert_eq!(
            builds,
            (20..20 + u64::try_from(MAX_REFUSED_BUILDS).unwrap()).collect::<Vec<_>>()
        );

        std::fs::remove_dir_all(l.build_dir("ny", 4)).unwrap();
        for build in 20..20 + u64::try_from(MAX_REFUSED_BUILDS).unwrap() {
            record_refused_shims(&l, "ay", build, &[]).unwrap();
        }
        let back = read_checked(&l).unwrap().expect("recorded");
        assert!(back.refused_shims.is_empty(), "{:?}", back.refused_shims);
        assert!(
            !std::fs::read_to_string(l.status())
                .unwrap()
                .contains("refused_shims"),
            "an empty map is not written"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn a_record_over_the_refused_shims_bounds_is_neither_written_nor_read() {
        let l = layout("refused-bounds");
        let entry = |build: u64| RefusedShims {
            build,
            names: vec!["pwsh".to_string()],
            count: 1,
        };
        let at_bounds = Status {
            schema: 1,
            refused_shims: (0..MAX_STATUS_PROGRAMS)
                .map(|i| (format!("p{i}"), vec![entry(1)]))
                .chain(std::iter::once((
                    "p0".to_string(),
                    (0..u64::try_from(MAX_REFUSED_BUILDS).unwrap())
                        .map(entry)
                        .collect(),
                )))
                .collect(),
            ..Default::default()
        };
        write(&l, &at_bounds).unwrap();
        assert_eq!(read_checked(&l).unwrap().as_ref(), Some(&at_bounds));
        let good = std::fs::read_to_string(l.status()).unwrap();

        let mut programs = at_bounds.clone();
        programs
            .refused_shims
            .insert("one-more".to_string(), vec![entry(1)]);
        let mut builds = at_bounds.clone();
        builds
            .refused_shims
            .get_mut("p1")
            .unwrap()
            .extend((2..=u64::try_from(MAX_REFUSED_BUILDS).unwrap() + 1).map(entry));
        for (what, over) in [("programs", &programs), ("builds", &builds)] {
            let err = write(&l, over).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{what}: {err}");
            assert_eq!(
                std::fs::read_to_string(l.status()).unwrap(),
                good,
                "{what}: a refused write leaves the record"
            );
            std::fs::write(l.status(), over.to_toml().unwrap()).unwrap();
            let err = read_checked(&l).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{what}: {err}");
            std::fs::write(l.status(), &good).unwrap();
        }
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn never_checked_holds_until_a_successful_pass_stamps_last_success_at() {
        let l = layout("never-checked");
        assert!(never_checked(&l), "no status.toml at all");
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
        let legacy = "schema = 1\nupdated_at = \"2026-08-01T00:00:00Z\"\nenabled = true\n\
             index_source = \"o/r\"\noutcome = \"up to date\"\n";
        std::fs::write(l.status(), legacy).unwrap();
        assert!(never_checked(&l), "pre-field record: empty last_success_at");
        stamp_success(&l, "2026-09-10T07:00:00Z").unwrap();
        assert!(!never_checked(&l));
        let back = read(&l).unwrap();
        assert_eq!(back.last_success_at, "2026-09-10T07:00:00Z");
        assert_eq!(back.updated_at, "2026-09-10T07:00:00Z");
        assert_eq!(back.outcome, "up to date", "the sentence is not touched");
        let text = std::fs::read_to_string(l.status()).unwrap();
        assert!(text.contains("last_success_at = \"2026-09-10T07:00:00Z\""));
        std::fs::write(l.status(), "schema = 1\nlast_success_at = \"  \"\n").unwrap();
        assert!(never_checked(&l));
        std::fs::remove_file(l.status()).unwrap();
        stamp_success(&l, "2026-09-10T08:00:00Z").unwrap();
        assert!(!never_checked(&l));
        assert_eq!(read(&l).unwrap().schema, 1);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn the_full_pass_rule_conforms_to_its_derived_model() {
        use super::full_pass_rule_tier1::{legacy_stamps, looks, realize, records};
        use aterm_models_atpkg::atpkg_full_pass_rule_model;
        use aterm_update_core::pkg_check;
        let model = atpkg_full_pass_rule_model();
        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let fixed = records(&model);
        assert!(fixed.len() > 20, "{}", fixed.len());
        crate::testdir::each_side_by_side(&fixed, |i, state| {
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
        });
        let old = records(&buggy);
        assert!(old.len() > 5, "{}", old.len());
        crate::testdir::each_side_by_side(&old, |i, state| {
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
        });
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

/// Tier-1 fixtures of `AtpkgFullPassRule` shared by this module's conformance test and `cli`'s.
#[cfg(test)]
pub(crate) mod full_pass_rule_tier1 {
    use super::*;
    use aterm_spec::derive::Model;
    use aterm_update_core::pkg_check::{PassOutcome, Stamps};

    pub(crate) type State = BTreeMap<&'static str, i64>;
    pub(crate) const AGE_SECS: [i64; 5] = [60, 15 * 60, 3 * 3600, 6 * 3600 + 60, 9 * 3600];
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

    /// One reachable state per distinct record.
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

    /// `state`'s record laid down at `now` in a fresh store by the real writers: the success ([`stamp_success`]).
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

    /// The reader [`pass_stamps`] replaced, kept as the negative control: the attempt is `updated_at`, else the success.
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
