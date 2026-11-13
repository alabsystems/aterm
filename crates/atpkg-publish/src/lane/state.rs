// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A lane's durable state.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use aterm_update_core::FileLock;

use super::LaneKind;
use super::report::Transcript;
use crate::api::Build;
use crate::error::{Error, Result};
use crate::triple::Triple;

/// A run holding the lock longer than this has stalled (a hung build, a prompt).
pub const STALLED_AFTER_SECS: i64 = 12 * 3600;

/// Attempt counters and notification stamps older than this are swept.
pub const PRUNE_AFTER_SECS: i64 = 30 * 86_400;

/// The tip lane's per-build evidence (step logs, receipts, retired records) older than this is
/// swept, except what a `BLOCKED` latch still points a person at: nearly every pass is a new
/// build, so what a pass leaves must not outlive its use.
pub const EVIDENCE_KEEP_SECS: i64 = 7 * 86_400;

const STAMP: &str = "# pins-resolved-for ";

/// Where one lane keeps its state.
#[derive(Clone, Debug)]
pub struct LaneState {
    /// Which lane.
    pub kind: LaneKind,
    /// The lanes' state root (`Ctx::state_dir`).
    pub root: PathBuf,
    /// This lane's own directory (attempts, latches, reports, scratch clones).
    pub dir: PathBuf,
    /// The index floor.
    pub index_seen: PathBuf,
    /// The pins floor.
    pub pins_seen: PathBuf,
    /// The single-instance lock.
    pub lock: PathBuf,
    /// The lock a queued staged-only pass holds: the lane's own, except on the tip lane, whose
    /// dispatcher must not wait behind a build its producer holds [`Self::lock`] for.
    pub publish_lock: PathBuf,
    /// The lane's log.
    pub log: PathBuf,
    /// When the run holding the lock started.
    pub started: PathBuf,
    /// Consecutive runs that could not read the channel head.
    pub head_failures: PathBuf,
}

/// What the lock answered.
pub enum Lock {
    /// This run holds it until the guard drops.
    Held(FileLock),
    /// Another run holds it.
    Busy,
}

impl LaneState {
    /// The directory watched by an explicitly installed staged-only dispatcher.
    #[must_use]
    pub fn ready_dir(&self) -> PathBuf {
        self.root.join("publish-ready")
    }

    /// Wake the staged-only dispatcher after a persisted stage. The stager also
    /// repeats this atomic hint after unlocking; it grants no publication authority.
    /// # Errors
    /// An unsafe queue directory or a failed atomic write.
    pub fn notify_ready(&self) -> Result<()> {
        let directory = self.ready_dir();
        aterm_types::fs_restricted::ensure_private_dir(&directory)
            .map_err(|e| Error::io("create the ready notification directory", &e))?;
        aterm_uds::atomic_file::write(&directory.join("pending"), b"staged\n", 0o600)
            .map_err(|e| Error::io("record staged work notification", &e))
    }

    /// Read only whether staged work requested a pass, never whether it qualifies.
    /// The caller holds the same lock as the staging writer.
    /// # Errors
    /// A non-directory notification root or malformed/nonregular notification.
    pub fn ready_pending(&self) -> Result<bool> {
        let directory = self.ready_dir();
        match std::fs::symlink_metadata(&directory) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
            Ok(_) => return Err(Error::refused("unsafe ready notification directory")),
            Err(e) => return Err(Error::io("inspect ready notification directory", &e)),
        }
        let pending = directory.join("pending");
        match std::fs::symlink_metadata(&pending) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => {}
            Ok(_) => return Err(Error::refused("unsafe ready notification file")),
            Err(e) => return Err(Error::io("inspect ready notification", &e)),
        }
        match crate::identity::read_regular_bytes(&pending, 16) {
            Ok(bytes) if bytes == b"staged\n" => Ok(true),
            Ok(_) => Err(Error::refused("malformed ready notification")),
            Err(e) => Err(Error::io("read ready notification", &e)),
        }
    }

    /// Retire a notification only after a completed pass, while still holding the
    /// lane lock. Failed and covered passes leave it for a later attempt.
    /// # Errors
    /// Removing the notification failed.
    pub fn clear_ready(&self) -> Result<()> {
        match std::fs::remove_file(self.ready_dir().join("pending")) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Error::io("retire staged work notification", &e)),
        }
    }

    /// The state of lane `kind` under `root`.
    #[must_use]
    pub fn of(kind: LaneKind, root: &Path) -> Self {
        match kind {
            LaneKind::Alab => {
                let dir = root.join("alab");
                Self {
                    kind,
                    root: root.to_path_buf(),
                    index_seen: dir.join("index-seen"),
                    pins_seen: dir.join("pins-seen"),
                    started: dir.join("run-started"),
                    head_failures: dir.join("head-failures"),
                    dir,
                    lock: root.join("alab.lockf"),
                    publish_lock: root.join("alab.lockf"),
                    log: root.join("alab.log"),
                }
            }
            LaneKind::Tip => {
                let dir = root.join("tip");
                Self {
                    kind,
                    root: root.to_path_buf(),
                    index_seen: dir.join("index-seen"),
                    pins_seen: dir.join("pins-seen"),
                    started: dir.join("run-started"),
                    head_failures: dir.join("head-failures"),
                    dir,
                    lock: root.join("tip.lockf"),
                    publish_lock: root.join("tip-publish.lockf"),
                    log: root.join("tip.log"),
                }
            }
            LaneKind::Linux => Self {
                kind,
                root: root.to_path_buf(),
                dir: root.to_path_buf(),
                index_seen: root.join("atpkg-index-seen"),
                pins_seen: root.join("atpkg-pins-seen"),
                started: root.join("atpkg-run-started"),
                head_failures: root.join("atpkg-head-failures"),
                lock: root.join("atpkg.lock"),
                publish_lock: root.join("atpkg.lock"),
                log: root.join("atpkg.log"),
            },
        }
    }

    /// Create the lane's directory.
    /// # Errors
    /// A could-not-run naming it.
    pub fn ensure(&self) -> Result<()> {
        std::fs::create_dir_all(&self.dir)
            .map_err(|e| Error::io(format!("mkdir {}", self.dir.display()), &e))
    }

    /// Take the single-instance lock without waiting.
    /// # Errors
    /// A could-not-run when the lock file cannot be opened.
    pub fn try_lock(&self) -> Result<Lock> {
        Self::lock_within(&self.lock, Duration::ZERO)
    }

    /// Give a staging writer time to release the same lock after emitting its
    /// notification. Longer contention remains covered by the existing timer.
    /// # Errors
    /// A could-not-run when the lock file cannot be opened.
    pub fn queued_lock(&self) -> Result<Lock> {
        Self::lock_within(&self.publish_lock, Duration::from_secs(30))
    }

    /// Take the queued pass's lock without waiting.
    /// # Errors
    /// A could-not-run when the lock file cannot be opened.
    pub fn try_publish_lock(&self) -> Result<Lock> {
        Self::lock_within(&self.publish_lock, Duration::ZERO)
    }

    fn lock_within(lock: &Path, timeout: Duration) -> Result<Lock> {
        if let Some(parent) = lock.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::io(format!("mkdir {}", parent.display()), &e))?;
        }
        match FileLock::acquire_within(lock, timeout) {
            Ok(guard) => Ok(Lock::Held(guard)),
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => Ok(Lock::Busy),
            Err(e) => Err(Error::io(format!("lock {}", lock.display()), &e)),
        }
    }

    /// Stamp the start of the run that holds the lock.
    pub fn mark_started(&self, now_unix: i64) {
        let _ = super::report::write_atomic(&self.started, &format!("{now_unix}\n"));
    }

    /// The run that held the lock ended.
    pub fn clear_started(&self) {
        let _ = std::fs::remove_file(&self.started);
    }

    /// When a busy lock's holder started, if that is longer ago than [`STALLED_AFTER_SECS`].
    #[must_use]
    pub fn stalled_since(&self, now_unix: i64) -> Option<i64> {
        let started = i64::try_from(read_number(&self.started).ok().flatten()?).ok()?;
        (now_unix.saturating_sub(started) > STALLED_AFTER_SECS).then_some(started)
    }

    /// The index this lane last processed (0 when it never ran).
    /// # Errors
    ///
    /// A refusal when the record is not a number — a floor that cannot be read is never
    /// read as 0.
    pub fn index_seen(&self) -> Result<u64> {
        read_number(&self.index_seen).map(|n| n.unwrap_or(0))
    }

    /// Refuse an index head below the one this lane last processed, returning the floor.
    /// # Errors
    /// The downgrade refusal.
    pub fn check_index(&self, head: u64) -> Result<u64> {
        let seen = self.index_seen()?;
        if head < seen {
            return Err(Error::refused(format!(
                "channel head is index {head} but this lane already processed {seen} — an \
                 index DOWNGRADE on the channel; refusing (investigate, or reset {} \
                 deliberately)",
                self.index_seen.display()
            )));
        }
        Ok(seen)
    }

    /// Hold `pins` (resolved for `targets`) to the pins this lane last saw.
    /// # Errors
    /// The pin-downgrade refusal; a refusal for an unreadable record.
    pub fn check_pins(
        &self,
        targets: &[Triple],
        pins: &BTreeMap<String, Build>,
        head: u64,
        tx: &Transcript,
    ) -> Result<()> {
        let Ok(text) =
            crate::identity::read_regular_text(&self.pins_seen, crate::identity::MAX_META_BYTES)
        else {
            return Ok(());
        };
        let stamp = stamp_of(targets);
        let first = text.lines().next().unwrap_or_default();
        let shell_apple = self.kind == LaneKind::Alab
            && !first.starts_with(STAMP)
            && stamp == stamp_of(super::alab::TARGETS);
        let skip = usize::from(!shell_apple);
        if !shell_apple && first != stamp {
            tx.warn(format!(
                "{} was not recorded for {} (it predates per-target pin resolution, or another \
                 target set wrote it) — skipping the pin-downgrade gate for THIS run and \
                 re-stamping it",
                file_name(&self.pins_seen),
                names(targets)
            ));
            return Ok(());
        }
        for line in text.lines().skip(skip) {
            if line.trim().is_empty() {
                continue;
            }
            let mut words = line.split_whitespace();
            let (Some(program), Some(build), None) = (words.next(), words.next(), words.next())
            else {
                return Err(Error::refused(format!(
                    "{} carries an unreadable line `{line}` — a floor that cannot be read is \
                     never skipped; fix or remove it deliberately",
                    self.pins_seen.display()
                )));
            };
            let seen: Build = (!build.is_empty() && build.bytes().all(|b| b.is_ascii_digit()))
                .then(|| build.parse().ok())
                .flatten()
                .ok_or_else(|| {
                    Error::refused(format!(
                        "{} carries an unreadable line `{line}` — a floor that cannot be read is \
                         never skipped; fix or remove it deliberately",
                        self.pins_seen.display()
                    ))
                })?;
            match pins.get(program) {
                None => tx.warn(format!(
                    "program {program} was pinned when this lane last ran and is ABSENT from \
                     index-{head} (deliberate retirement, or a truncated index upstream)"
                )),
                Some(now) if *now < seen => {
                    return Err(Error::refused(format!(
                        "PIN DOWNGRADE on the channel: {program} {seen} -> {now} in \
                         index-{head} — refusing everything (a per-program rollback must move \
                         min_build or yanked, never silently lower a pin)"
                    )));
                }
                Some(_) => {}
            }
        }
        Ok(())
    }

    /// Raise the floors to at least `index` and `pins`, never below what is recorded: for a
    /// lane whose producer and dispatcher both record, so the one that read the older head
    /// cannot lower what the other left. A program is kept at the higher of its two builds; a
    /// record another target set wrote is replaced.
    /// # Errors
    /// A refusal for an unreadable floor; a could-not-run naming the file.
    pub fn raise(
        &self,
        index: u64,
        targets: &[Triple],
        pins: &BTreeMap<String, Build>,
    ) -> Result<()> {
        let index = index.max(self.index_seen()?);
        let mut merged = pins.clone();
        if let Ok(text) =
            crate::identity::read_regular_text(&self.pins_seen, crate::identity::MAX_META_BYTES)
            && text.lines().next() == Some(stamp_of(targets).as_str())
        {
            for line in text.lines().skip(1) {
                let mut words = line.split_whitespace();
                if let (Some(program), Some(Ok(build)), None) = (
                    words.next(),
                    words.next().map(str::parse::<Build>),
                    words.next(),
                ) {
                    let cell = merged.entry(program.to_string()).or_insert(build);
                    *cell = (*cell).max(build);
                }
            }
        }
        self.record(index, targets, &merged)
    }

    /// Record the floors this run leaves.
    /// # Errors
    /// A could-not-run naming the file.
    pub fn record(
        &self,
        index: u64,
        targets: &[Triple],
        pins: &BTreeMap<String, Build>,
    ) -> Result<()> {
        use std::fmt::Write as _;
        super::report::write_atomic(&self.index_seen, &format!("{index}\n"))?;
        let mut text = stamp_of(targets);
        text.push('\n');
        for (p, b) in pins {
            let _ = writeln!(text, "{p} {b}");
        }
        super::report::write_atomic(&self.pins_seen, &text)
    }

    /// One more run that could not read the channel head; the count so far.
    #[must_use]
    pub fn bump_head_failures(&self, dry_run: bool) -> u64 {
        let n = read_number(&self.head_failures)
            .ok()
            .flatten()
            .unwrap_or(0)
            .saturating_add(1);
        if !dry_run {
            let _ = super::report::write_atomic(&self.head_failures, &format!("{n}\n"));
        }
        n
    }

    /// The channel head answered again.
    pub fn clear_head_failures(&self) {
        let _ = std::fs::remove_file(&self.head_failures);
    }

    /// The Apple lanes' attempt counter for one build.
    #[must_use]
    pub fn attempts_file(&self, program: &str, build: Build) -> PathBuf {
        self.dir.join(format!("attempts-{program}-{build}"))
    }

    /// The Apple lanes' BLOCKED latch for one build.
    #[must_use]
    pub fn blocked_file(&self, program: &str, build: Build) -> PathBuf {
        self.dir.join(format!("BLOCKED-{program}-{build}.md"))
    }

    /// One more failed pack of `program` `build`; the count so far.
    #[must_use]
    pub fn bump_attempts(&self, program: &str, build: Build) -> u32 {
        bump(&self.attempts_file(program, build))
    }

    /// Failed packs of `program` `build` so far.
    #[must_use]
    pub fn attempts(&self, program: &str, build: Build) -> u32 {
        count(&self.attempts_file(program, build))
    }

    /// The tip dispatcher's counter of one queued build's refused publishes and re-stagings.
    #[must_use]
    pub fn publish_attempts_file(&self, program: &str, build: Build) -> PathBuf {
        self.dir.join(format!("publish-attempts-{program}-{build}"))
    }

    /// One more refused publish (or re-staging) of `program` `build`; the count so far.
    #[must_use]
    pub fn bump_publish_attempts(&self, program: &str, build: Build) -> u32 {
        bump(&self.publish_attempts_file(program, build))
    }

    /// Refused publishes (and re-stagings) of `program` `build` so far.
    #[must_use]
    pub fn publish_attempts(&self, program: &str, build: Build) -> u32 {
        count(&self.publish_attempts_file(program, build))
    }

    /// The build packed: its counter goes.
    pub fn clear_attempts(&self, program: &str, build: Build) {
        let _ = std::fs::remove_file(self.attempts_file(program, build));
    }

    /// Sweep attempt counters older than [`PRUNE_AFTER_SECS`].
    pub fn prune(&self, now_unix: i64) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        for e in entries.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if !(name.starts_with("attempts-") || name.starts_with("publish-attempts-"))
                || !e.file_type().is_ok_and(|ft| ft.is_file())
            {
                continue;
            }
            if age(&e, now_unix).is_some_and(|a| a > PRUNE_AFTER_SECS) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }

    /// Sweep the tip lane's per-build evidence older than [`EVIDENCE_KEEP_SECS`]: a build's
    /// step logs (`logs/<program>-<build>/`), its receipt (`receipts/<program>-<build>.json`)
    /// and retired ready records (`retired/`). What a `BLOCKED-<program>-<build>.md` latch
    /// names is kept for as long as the latch stands.
    pub fn prune_evidence(&self, now_unix: i64) {
        let latched = |stem: &str| self.dir.join(format!("BLOCKED-{stem}.md")).exists();
        for (folder, suffix) in [("logs", ""), ("receipts", ".json"), ("retired", "")] {
            let Ok(entries) = std::fs::read_dir(self.dir.join(folder)) else {
                continue;
            };
            for e in entries.flatten() {
                let name = e.file_name();
                let name = name.to_string_lossy();
                // `<program>-<build>`: the whole name of a log folder, a receipt's stem, and
                // what a retired record's name starts with.
                let stem = name
                    .strip_suffix(suffix)
                    .unwrap_or(&name)
                    .split(super::ready::SUFFIX)
                    .next()
                    .unwrap_or_default();
                if latched(stem) || !age(&e, now_unix).is_some_and(|a| a > EVIDENCE_KEEP_SECS) {
                    continue;
                }
                let path = e.path();
                let _ = if e.file_type().is_ok_and(|ft| ft.is_dir()) {
                    std::fs::remove_dir_all(&path)
                } else {
                    std::fs::remove_file(&path)
                };
            }
        }
    }
}

/// How long ago `entry` was last written, by its modification time.
fn age(entry: &std::fs::DirEntry, now_unix: i64) -> Option<i64> {
    entry
        .metadata()
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .map(|mtime| now_unix.saturating_sub(mtime))
}

fn count(file: &Path) -> u32 {
    u32::try_from(read_number(file).ok().flatten().unwrap_or(0)).unwrap_or(u32::MAX)
}

fn bump(file: &Path) -> u32 {
    let n = count(file).saturating_add(1);
    let _ = super::report::write_atomic(file, &format!("{n}\n"));
    n
}

fn stamp_of(targets: &[Triple]) -> String {
    format!("{STAMP}{}", names(targets))
}

/// `a b c` — targets for a message or a stamp.
#[must_use]
pub fn names(targets: &[Triple]) -> String {
    targets
        .iter()
        .map(|t| t.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into(),
    )
}

fn read_number(path: &Path) -> Result<Option<u64>> {
    match crate::identity::read_regular_text(path, 4096) {
        Ok(text) => {
            let trimmed = text.trim();
            (!trimmed.is_empty() && trimmed.bytes().all(|b| b.is_ascii_digit()))
                .then(|| trimmed.parse().ok())
                .flatten()
                .map(Some)
                .ok_or_else(|| {
                    Error::refused(format!(
                        "{} holds `{trimmed}`, not a number — a floor that cannot be read is \
                         never read as 0",
                        path.display()
                    ))
                })
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::io(format!("read {}", path.display()), &e)),
    }
}
