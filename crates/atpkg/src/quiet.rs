// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Flip when quiet.

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::store::Layout;

/// How long an unattended flip waits for quiet, at most.
pub const CEILING_SECS: i64 = 4 * 60 * 60;

/// How often a [`Recheck`] host looks at a recorded deferral ([`due`]).
pub const RECHECK: std::time::Duration = std::time::Duration::from_secs(60);

/// `<prefix>/flip-deferred.toml`.
#[must_use]
pub fn record_path(layout: &Layout) -> PathBuf {
    layout.prefix.join("flip-deferred.toml")
}

/// Whether a pass may hold the toolchain's flip for quiet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlipPolicy {
    /// Flip at once.
    Now,
    /// The unattended lanes.
    WhenQuiet,
}

/// What a pass's flip decision reads.
pub struct FlipGate<'a> {
    /// Whether this pass may wait at all.
    pub policy: FlipPolicy,
    /// Unix seconds now.
    pub now: i64,
    /// The process table, as [`crate::gc::running_from`] takes it.
    pub running: &'a dyn Fn() -> Option<Vec<PathBuf>>,
}

/// The table a [`FlipGate::NOW`] never reads.
const fn no_table() -> Option<Vec<PathBuf>> {
    None
}

impl FlipGate<'static> {
    /// Flip at once.
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
    /// A live process runs from it.
    Running { exe: PathBuf },
    /// It could not be told.
    Unknown { why: String },
}

impl Busy {
    /// Whether the flip may land now.
    #[must_use]
    pub const fn is_quiet(&self) -> bool {
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
    /// Wait for quiet.
    Defer { since: i64 },
    /// The wait reached [`CEILING_SECS`] (it started at `since`).
    FlipAtCeiling { since: i64 },
}

/// The decision, pure.
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

/// Whether the toolchain whose live builds are `live` (member → build) is in use.
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
    for dir in dirs {
        if let Some(exe) = crate::gc::running_from_table(&dir, &table) {
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
    /// Unix seconds of the first deferral of this wait.
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
    /// Member → the file name of its staged archive in `staging/<member>/`.
    #[serde(default)]
    pub assets: BTreeMap<String, String>,
}

/// What a held flip moves, as its record keeps it ([`note_deferred`]).
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
    pub const fn ceiling_at(&self) -> i64 {
        self.since.saturating_add(CEILING_SECS)
    }

    /// The moves, as a person reads them.
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

    /// The doctor's line.
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
            match (stamp.get(..10), stamp.get(11..16)) {
                (Some(day), Some(time)) => format!("{day} {time} UTC"),
                _ => stamp,
            }
        }
        _ => "an unknown time".to_string(),
    }
}

/// The recorded deferral, if one stands.
#[must_use]
pub fn read(layout: &Layout) -> Option<Deferral> {
    let text =
        crate::metadata_io::read_bounded_regular_utf8(&record_path(layout), MAX_RECORD_BYTES)
            .ok()?;
    aterm_toml::from_str::<Deferral>(&text).ok().filter(|d| {
        d.schema == SCHEMA
            && !d.group.is_empty()
            && !d.group.chars().any(char::is_control)
            && !d.why.chars().any(char::is_control)
            && d.from
                .keys()
                .chain(d.to.keys())
                .chain(d.assets.keys())
                .all(|k| crate::ops::uninstall_name_shape(k).is_ok())
    })
}

/// Publish the record with durable bytes and directory entry.
///
/// # Errors
/// The render's or the write's.
pub(crate) fn write(layout: &Layout, record: &Deferral) -> io::Result<()> {
    if std::fs::symlink_metadata(&layout.prefix).is_ok_and(|m| !m.file_type().is_dir()) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "prefix is not a real directory",
        ));
    }
    let text = aterm_toml::to_string(record).map_err(|e| io::Error::other(e.to_string()))?;
    aterm_uds::atomic_file::write(&record_path(layout), text.as_bytes(), 0o600)
}

/// Drop the record.
pub(crate) fn clear(layout: &Layout) {
    let path = record_path(layout);
    if std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
        let _ = std::fs::remove_file(path);
    }
}

/// Record a deferral of `group` at `now`, preserving `since` from the first deferral of this wait.
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
/// the store.
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
    /// The record describes a wait.
    Waits(Deferral),
    /// The record outlived every build it moves off (a store emptied by hand) and waits for nothing.
    Outlived(Deferral),
}

/// The deferral record as `aterm pkg doctor` reports it.
#[must_use]
pub fn held(layout: &Layout) -> Held {
    match read(layout) {
        None => Held::Nothing,
        Some(record) if stands(layout, &record) => Held::Waits(record),
        Some(record) => Held::Outlived(record),
    }
}

/// Retire the record under the store lock when it outlived every build it moves off.
pub(crate) fn end_outlived(layout: &Layout) -> bool {
    if !read(layout).is_some_and(|record| !stands(layout, &record)) {
        return false;
    }
    clear(layout);
    read(layout).is_none()
}

/// The archives a standing record's flip waits on (member → file name in `staging/<member>/`), which every gc spares.
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

/// Why a [`Recheck`] host should wake a pass for a held toolchain move ([`due`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Due {
    /// Nothing is using the toolchain now.
    Quiet,
    /// The wait reached its ceiling.
    Ceiling,
    /// A rustup view a flip left behind is used by nothing now.
    View,
}

/// A recorded deferral that is due, and which look of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DueFlip {
    /// Why it is due.
    pub why: Due,
    /// The record's `checked` stamp at this look.
    pub checked: i64,
}

/// Whether a held toolchain move is due now.
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

/// `<prefix>/view-left.toml`.
#[must_use]
pub fn view_record_path(layout: &Layout) -> PathBuf {
    layout.prefix.join("view-left.toml")
}

/// A rustup view left behind, recorded so it is laid as soon as nothing uses it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewLeft {
    /// The build the view was not re-laid onto.
    pub build: String,
    /// Unix seconds the view was first left behind in this stretch.
    pub since: i64,
    /// Unix seconds of the pass that last left it.
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

/// The recorded views left behind.
#[must_use]
pub fn views_left(layout: &Layout) -> ViewsLeft {
    let mut out =
        crate::metadata_io::read_bounded_regular_utf8(&view_record_path(layout), MAX_RECORD_BYTES)
            .ok()
            .and_then(|text| aterm_toml::from_str::<ViewsLeft>(&text).ok())
            .filter(|r| r.schema == SCHEMA)
            .unwrap_or_default();
    out.views.retain(|name, v| {
        crate::seam::name_allowed(name)
            && !v.build.is_empty()
            && !v.build.chars().any(char::is_control)
            && !v.why.chars().any(char::is_control)
    });
    out
}

/// Publish `record` durably, or remove the file when it holds no view.
fn write_views_left(layout: &Layout, record: &ViewsLeft) -> io::Result<()> {
    if std::fs::symlink_metadata(&layout.prefix).is_ok_and(|m| !m.file_type().is_dir()) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "prefix is not a real directory",
        ));
    }
    let dest = view_record_path(layout);
    if record.views.is_empty() {
        if !std::fs::symlink_metadata(&dest).is_ok_and(|m| m.file_type().is_file()) {
            return Ok(());
        }
        return match std::fs::remove_file(&dest) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    let text = aterm_toml::to_string(record).map_err(|e| io::Error::other(e.to_string()))?;
    aterm_uds::atomic_file::write(&dest, text.as_bytes(), 0o600)
}

/// Record that a pass left the view `name` as it stood, not re-laid onto `build`, at `now`.
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

/// Rustup is gone, so every view left behind ends.
pub(crate) fn clear_views_left(layout: &Layout) {
    let _ = write_views_left(layout, &ViewsLeft::default());
}

/// The view `name` is as it should be.
pub(crate) fn clear_view_left(layout: &Layout, name: &str) {
    let mut record = views_left(layout);
    if record.views.remove(name).is_some() {
        let _ = write_views_left(layout, &record);
    }
}

/// [`due_with`]'s half for the views a flip left behind: the first recorded view that is
/// laid and unused (no lease, no process running from it) is due, keyed by its entry's
/// `checked` stamp.
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

/// The re-check of a held toolchain move, as every host runs it (the window's package park, [`crate::vendor_direct::watch`]'s runner).
#[derive(Clone, Debug, Default)]
pub struct Recheck {
    /// When [`Self::wake`] last looked.
    last: Option<std::time::Instant>,
    /// The record look the last look found quiet (the first of the two a wake needs):
    /// whether it was a view's, and its stamp.
    quiet_seen: Option<(bool, i64)>,
    /// The record look the last wake was for: whether it was a view's, and its stamp.
    woke_for: Option<(bool, i64)>,
}

impl Recheck {
    /// Whether a look is due, without reading the store or taking a watch claim.
    #[must_use]
    pub fn look_due(&self, now: std::time::Instant) -> bool {
        self.last
            .is_none_or(|at| now.saturating_duration_since(at) >= RECHECK)
    }

    /// Whether to wake a pass now, `due` asked only when a look is due at `now` (a monotonic instant).
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
#[path = "quiet_tests.rs"]
mod tests;
