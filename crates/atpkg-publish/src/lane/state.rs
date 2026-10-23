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

/// The first line of a pins record: the targets it was resolved for. A build number is
/// comparable only to one resolved the same way.
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
                    log: root.join("alab.log"),
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
        if let Some(parent) = self.lock.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::io(format!("mkdir {}", parent.display()), &e))?;
        }
        match FileLock::acquire_within(&self.lock, Duration::ZERO) {
            Ok(guard) => Ok(Lock::Held(guard)),
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => Ok(Lock::Busy),
            Err(e) => Err(Error::io(format!("lock {}", self.lock.display()), &e)),
        }
    }

    /// Stamp the start of the run that holds the lock.
    pub fn mark_started(&self, now_unix: i64) {
        let _ = std::fs::write(&self.started, format!("{now_unix}\n"));
    }

    /// The run that held the lock ended.
    pub fn clear_started(&self) {
        let _ = std::fs::remove_file(&self.started);
    }

    /// When a busy lock's holder started, if that is longer ago than [`STALLED_AFTER_SECS`].
    #[must_use]
    pub fn stalled_since(&self, now_unix: i64) -> Option<i64> {
        let started = i64::try_from(read_number(&self.started).ok().flatten()?).ok()?;
        (now_unix - started > STALLED_AFTER_SECS).then_some(started)
    }

    /// The index this lane last processed (0 when it never ran).
    /// # Errors
    ///
    /// A refusal when the record is not a number — a floor that cannot be read is never
    /// read as 0.
    pub fn index_seen(&self) -> Result<u64> {
        read_number(&self.index_seen).map(|n| n.unwrap_or(0))
    }

    /// Refuse an index head below the one this lane last processed.
    /// # Errors
    /// The downgrade refusal.
    pub fn check_index(&self, head: u64) -> Result<()> {
        let seen = self.index_seen()?;
        if head < seen {
            return Err(Error::refused(format!(
                "channel head is index {head} but this lane already processed {seen} — an \
                 index DOWNGRADE on the channel; refusing (investigate, or reset {} \
                 deliberately)",
                self.index_seen.display()
            )));
        }
        Ok(())
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
        let Ok(text) = std::fs::read_to_string(&self.pins_seen) else {
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
            let mut words = line.split_whitespace();
            let (Some(program), Some(build)) = (words.next(), words.next()) else {
                continue;
            };
            let seen: Build = build.parse().map_err(|_| {
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

    /// Record the floors this run leaves.
    /// # Errors
    /// A could-not-run naming the file.
    pub fn record(
        &self,
        index: u64,
        targets: &[Triple],
        pins: &BTreeMap<String, Build>,
    ) -> Result<()> {
        super::report::write_atomic(&self.index_seen, &format!("{index}\n"))?;
        let mut text = stamp_of(targets);
        text.push('\n');
        for (p, b) in pins {
            text.push_str(&format!("{p} {b}\n"));
        }
        super::report::write_atomic(&self.pins_seen, &text)
    }

    /// One more run that could not read the channel head; the count so far.
    #[must_use]
    pub fn bump_head_failures(&self, dry_run: bool) -> u64 {
        let n = read_number(&self.head_failures).ok().flatten().unwrap_or(0) + 1;
        if !dry_run {
            let _ = std::fs::write(&self.head_failures, format!("{n}\n"));
        }
        n
    }

    /// The channel head answered again.
    pub fn clear_head_failures(&self) {
        let _ = std::fs::remove_file(&self.head_failures);
    }

    /// The Apple lane's attempt counter for one build.
    #[must_use]
    pub fn attempts_file(&self, program: &str, build: Build) -> PathBuf {
        self.dir.join(format!("attempts-{program}-{build}"))
    }

    /// The Apple lane's BLOCKED latch for one build.
    #[must_use]
    pub fn blocked_file(&self, program: &str, build: Build) -> PathBuf {
        self.dir.join(format!("BLOCKED-{program}-{build}.md"))
    }

    /// One more failed pack of `program` `build`; the count so far.
    #[must_use]
    pub fn bump_attempts(&self, program: &str, build: Build) -> u32 {
        let f = self.attempts_file(program, build);
        let n = read_number(&f).ok().flatten().unwrap_or(0) + 1;
        let _ = std::fs::write(&f, format!("{n}\n"));
        u32::try_from(n).unwrap_or(u32::MAX)
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
            if !name.to_string_lossy().starts_with("attempts-") {
                continue;
            }
            let age = e
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .and_then(|d| i64::try_from(d.as_secs()).ok())
                .map(|mtime| now_unix - mtime);
            if age.is_some_and(|a| a > PRUNE_AFTER_SECS) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
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
    match std::fs::read_to_string(path) {
        Ok(text) => text.trim().parse().map(Some).map_err(|_| {
            Error::refused(format!(
                "{} holds `{}`, not a number — a floor that cannot be read is never read as 0",
                path.display(),
                text.trim()
            ))
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::io(format!("read {}", path.display()), &e)),
    }
}
