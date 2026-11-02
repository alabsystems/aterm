// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Durable coverage for the legacy-mirror decision; `.heard` remains advisory telemetry.

use super::{HEARD_TTL_SECS, MAX_HEARD, MAX_VAULTS};
use crate::{Error, fsx};
use aterm_uds::private_file::PrivateLock;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub(super) type History = BTreeMap<String, VaultHistory>;
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub(super) struct VaultHistory {
    pub(super) subscribers: BTreeMap<String, u64>,
    pub(super) anonymous: Option<u64>,
}
const WINDOW: u64 = crate::transport::HEARD_COVERAGE_SECS;
const SNAPSHOT: &str = "heard-coverage";
const VERSION: &str = "aterm-heard-coverage-v2";
const MAX_SNAPSHOT: usize = MAX_VAULTS * (MAX_HEARD + 1) * 160 + 256;
type Writer = fn(&Path, &[u8], u32) -> crate::Result<()>;

pub(super) struct Coverage {
    since: u64,
    checkpoint: u64,
    path: PathBuf,
    writer: Writer,
    // The stable sidecar is never renamed with the snapshot it protects.
    _lock: PrivateLock,
}

impl Coverage {
    /// Mark this run dirty before it can answer tracked reads. Only a complete, clean snapshot
    /// carries coverage through a restart; old telemetry is never evidence of completeness.
    pub(super) fn begin(store: &Path, now: u64, history: &mut History) -> crate::Result<Self> {
        Self::begin_using(store, now, history, write_snapshot)
    }

    fn begin_using(
        store: &Path,
        now: u64,
        history: &mut History,
        writer: Writer,
    ) -> crate::Result<Self> {
        let lock = PrivateLock::take(
            store,
            "heard-coverage.lock",
            Duration::ZERO,
            "another vault server holds the heard-history store",
        )?;
        let path = store.join(SNAPSHOT);
        let saved = fsx::read_owned(&path, MAX_SNAPSHOT)?
            .as_deref()
            .and_then(decode);
        let mut since = now;
        if let Some((clean, previous_since, checkpoint, saved)) = saved {
            if clean && previous_since != 0 && now >= checkpoint {
                since = previous_since;
                *history = saved;
            } else {
                for (vault, rows) in saved {
                    let held = history.entry(vault).or_default();
                    held.anonymous = held.anonymous.max(rows.anonymous);
                    for (recipient, at) in rows.subscribers {
                        let last = held.subscribers.entry(recipient).or_default();
                        *last = (*last).max(at);
                    }
                }
            }
        }
        if bound(history, now) {
            since = now;
        }
        let coverage = Self {
            since,
            checkpoint: now,
            path,
            writer,
            _lock: lock,
        };
        coverage.write(false, history)?;
        Ok(coverage)
    }

    pub(super) fn invalidate(&mut self, now: u64) {
        self.since = now;
        self.checkpoint = now;
    }

    pub(super) fn observe_time(&mut self, now: u64) {
        if now == 0 || now < self.checkpoint || self.since == 0 {
            self.since = now;
        }
        self.checkpoint = now;
    }

    pub(super) fn complete_since(&mut self, now: u64) -> Option<u64> {
        self.observe_time(now);
        (self.since != 0 && now.saturating_sub(self.since) >= WINDOW).then_some(self.since)
    }

    /// The stop fence has excluded every later tracked read before this is called. A failed
    /// write leaves a dirty epoch (or a complete clean snapshot if only its directory flush
    /// failed); neither can invent an observation missing from these rows.
    pub(super) fn finish(mut self, history: &History, now: u64) -> crate::Result<()> {
        self.observe_time(now);
        self.write(true, history)
    }

    fn write(&self, clean: bool, history: &History) -> crate::Result<()> {
        let rows = encode_rows(history);
        if rows.len() + 256 > MAX_SNAPSHOT {
            return Err(Error::new("heard-history snapshot exceeds its bound"));
        }
        let count: usize = history
            .values()
            .map(|rows| rows.subscribers.len() + usize::from(rows.anonymous.is_some()))
            .sum();
        let status = if clean { "clean" } else { "dirty" };
        let metadata = format!(
            "{VERSION} {status} {} {} {count}",
            self.since, self.checkpoint
        );
        let digest = aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(
            format!("{metadata}\n{rows}").as_bytes(),
        ));
        let text = format!("{metadata} {digest}\n{rows}");
        (self.writer)(&self.path, text.as_bytes(), 0o600)
    }
}

fn write_snapshot(path: &Path, bytes: &[u8], mode: u32) -> crate::Result<()> {
    fsx::write_atomic(path, bytes, mode)?;
    // Atomic writes may accept a volume that cannot flush directory entries. A DIRTY epoch
    // must survive a power loss before any read is served, so this writer requires the flush.
    let parent = path
        .parent()
        .ok_or_else(|| Error::new("heard snapshot has no store directory"))?;
    aterm_durable::durable_dir(parent)?;
    Ok(())
}

fn encode_rows(history: &History) -> String {
    let mut text = String::new();
    for (vault, rows) in history {
        if let Some(at) = rows.anonymous {
            text.push_str(&format!("{vault} - {at}\n"));
        }
        for (recipient, at) in &rows.subscribers {
            text.push_str(&format!("{vault} {recipient} {at}\n"));
        }
    }
    text
}

fn decode(bytes: &[u8]) -> Option<(bool, u64, u64, History)> {
    let text = std::str::from_utf8(bytes).ok()?;
    let (head, rows) = text.split_once('\n')?;
    let words: Vec<_> = head.split_ascii_whitespace().collect();
    let [VERSION, status, since, checkpoint, count, digest] = words.as_slice() else {
        return None;
    };
    let clean = match *status {
        "clean" => true,
        "dirty" => false,
        _ => return None,
    };
    let since = since.parse::<u64>().ok()?;
    let checkpoint = checkpoint.parse::<u64>().ok()?;
    let count = count.parse::<usize>().ok()?;
    let metadata = format!("{VERSION} {status} {since} {checkpoint} {count}");
    if since > checkpoint
        || count > MAX_VAULTS * (MAX_HEARD + 1)
        || *digest
            != aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(
                format!("{metadata}\n{rows}").as_bytes(),
            ))
    {
        return None;
    }
    let mut history = History::new();
    let mut seen = 0;
    for row in rows.lines() {
        let words: Vec<_> = row.split_ascii_whitespace().collect();
        let [vault, recipient, at] = words.as_slice() else {
            return None;
        };
        if !crate::envelope::valid_name(vault)
            || (*recipient != "-" && crate::age::Recipient::parse(recipient).is_err())
        {
            return None;
        }
        let at = at.parse::<u64>().ok()?;
        let entries = history.entry((*vault).to_owned()).or_default();
        if *recipient == "-" {
            if entries.anonymous.replace(at).is_some() {
                return None;
            }
        } else if entries
            .subscribers
            .insert((*recipient).to_owned(), at)
            .is_some()
            || entries.subscribers.len() > MAX_HEARD
        {
            return None;
        }
        seen += 1;
    }
    (seen == count && history.len() <= MAX_VAULTS).then_some((clean, since, checkpoint, history))
}

/// Bound telemetry without using a dropped in-window observation as proof of absence.
pub(super) fn bound(history: &mut History, now: u64) -> bool {
    let mut lost_recent = false;
    for rows in history.values_mut() {
        rows.anonymous = rows
            .anonymous
            .filter(|at| now.saturating_sub(*at) <= HEARD_TTL_SECS);
        rows.subscribers
            .retain(|_, at| now.saturating_sub(*at) <= HEARD_TTL_SECS);
        while rows.subscribers.len() > MAX_HEARD {
            let Some((recipient, at)) = rows
                .subscribers
                .iter()
                .min_by_key(|(_, at)| **at)
                .map(|(r, at)| (r.clone(), *at))
            else {
                break;
            };
            lost_recent |= now.saturating_sub(at) <= WINDOW;
            rows.subscribers.remove(&recipient);
        }
    }
    history.retain(|_, rows| !rows.subscribers.is_empty() || rows.anonymous.is_some());
    while history.len() > MAX_VAULTS {
        let Some(vault) = history
            .iter()
            .min_by_key(|(_, rows)| {
                rows.subscribers
                    .values()
                    .copied()
                    .chain(rows.anonymous)
                    .max()
                    .unwrap_or(0)
            })
            .map(|(v, _)| v.clone())
        else {
            break;
        };
        if let Some(rows) = history.remove(&vault) {
            lost_recent |= rows
                .subscribers
                .values()
                .copied()
                .chain(rows.anonymous)
                .any(|at| now.saturating_sub(at) <= WINDOW);
        }
    }
    lost_recent
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn failed_epoch_writes_never_admit_a_run_or_create_a_clean_restart() {
        static FAIL: AtomicBool = AtomicBool::new(true);
        fn write(path: &Path, bytes: &[u8], mode: u32) -> crate::Result<()> {
            if FAIL.load(Ordering::SeqCst) {
                return Err(Error::new("injected snapshot flush failure"));
            }
            write_snapshot(path, bytes, mode)
        }
        let root = aterm_tempfile::tempdir().unwrap();
        let mut history = History::new();
        let start = 1_800_000_000;
        {
            let _unavailable = aterm_durable::seam::FailingDirs::with(aterm_durable::seam::ENOTSUP);
            assert!(
                Coverage::begin(root.path(), start, &mut history).is_err(),
                "an unavailable directory flush admitted a run"
            );
        }
        // The failed flush may already have renamed DIRTY, but no read was admitted.
        std::fs::remove_file(root.path().join(SNAPSHOT)).unwrap();
        assert!(Coverage::begin_using(root.path(), start, &mut history, write).is_err());
        assert!(
            !root.path().join(SNAPSHOT).exists(),
            "a failed DIRTY write admitted a run"
        );
        FAIL.store(false, Ordering::SeqCst);
        let mut coverage = Coverage::begin_using(root.path(), start, &mut history, write).unwrap();
        assert_eq!(coverage.complete_since(start + WINDOW), Some(start));
        let dirty = std::fs::read(root.path().join(SNAPSHOT)).unwrap();
        FAIL.store(true, Ordering::SeqCst);
        assert!(coverage.finish(&history, start + WINDOW).is_err());
        assert_eq!(std::fs::read(root.path().join(SNAPSHOT)).unwrap(), dirty);
        let mut restarted = Coverage::begin(root.path(), start + WINDOW + 1, &mut history).unwrap();
        assert_eq!(
            restarted.complete_since(start + WINDOW + 1),
            None,
            "a failed CLEAN write preserved coverage"
        );
        assert_eq!(
            restarted.complete_since(start + 2 * WINDOW + 1),
            Some(start + WINDOW + 1)
        );
        restarted.finish(&history, start + 2 * WINDOW + 1).unwrap();
    }
}
