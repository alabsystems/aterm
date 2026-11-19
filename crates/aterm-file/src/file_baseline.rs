// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Baselines an editable export can still name after a projection or encoding change.

use super::{File, MAX_BYTES, read_bounded};
use crate::{Snapshot, Status};
use std::{io, path::PathBuf};

impl File {
    fn alternate_baseline(&self, rev: u64, digest: &str) -> PathBuf {
        self.root
            .baselines(&self.name)
            .join(format!("{rev}.{digest}.base"))
    }

    fn retained_render(&self, rev: u64) -> PathBuf {
        self.root
            .baselines(&self.name)
            .join(format!("{rev}.render"))
    }

    fn render_receipt(&self, rev: u64) -> PathBuf {
        self.root
            .baselines(&self.name)
            .join(format!("{rev}.receipt"))
    }

    /// An unstamped face names a revision, without an exported content digest. A prior
    /// rendering can stand for its raw grid only under the recording pass's receipt for both.
    #[must_use]
    pub fn snapshot_for_grid(&self, rev: u64, grid: &[u8]) -> Option<Snapshot> {
        if let Some(state) = self.state().filter(|state| state.rev == rev)
            && let Some(snapshot) = self
                .snapshot()
                .ok()
                .flatten()
                .filter(|snapshot| snapshot.rev == rev)
            && state
                .snapshot
                .as_ref()
                .is_none_or(|digest| aterm_codec::hex::encode_lower(&snapshot.content()) == *digest)
        {
            return Some(snapshot);
        }
        let receipt = read_bounded(&self.render_receipt(rev), 129)
            .ok()
            .flatten()?;
        let (baseline, content) = std::str::from_utf8(&receipt).ok()?.split_once(' ')?;
        if baseline != self.baseline_digest(rev, grid) || !aterm_codec::hex::is_lower(content, 64) {
            return None;
        }
        self.snapshot_for_export(rev, content)
    }

    /// The exact bytes this replica retained for an exported revision and digest.
    #[must_use]
    pub fn baseline_for_export(&self, rev: u64, digest: &str) -> Option<Vec<u8>> {
        if !aterm_codec::hex::is_lower(digest, 64) {
            return None;
        }
        [
            self.alternate_baseline(rev, digest),
            self.root.baseline(&self.name, rev),
        ]
        .into_iter()
        .filter_map(|path| read_bounded(&path, MAX_BYTES).ok().flatten())
        .find(|bytes| self.baseline_digest(rev, bytes) == digest)
    }

    /// Recover the exact displayed cells of an export whose baseline does not carry them.
    #[must_use]
    pub fn snapshot_for_export(&self, rev: u64, content: &str) -> Option<Snapshot> {
        let matches = |snapshot: &Snapshot| {
            snapshot.rev == rev
                && snapshot.name == self.name
                && aterm_codec::hex::encode_lower(&snapshot.content()) == content
        };
        let retained = read_bounded(&self.retained_render(rev), MAX_BYTES)
            .ok()
            .flatten()
            .and_then(|bytes| Snapshot::decode(&bytes).ok())
            .filter(matches);
        retained.or_else(|| self.snapshot().ok().flatten().filter(matches))
    }

    /// The published rows, their status, and the stamp binding their baseline and content,
    /// read together under the lock a pass records under.
    pub(crate) fn editable_snapshot(&self) -> io::Result<Option<(Snapshot, Status, String)>> {
        #[cfg(any(unix, windows))]
        let _lock = match aterm_uds::file_lock::LockFile::take_existing(
            &self.root.lock(&self.name),
            std::time::Instant::now() + std::time::Duration::from_secs(10),
            "another pass is recording this shared file",
        ) {
            Ok(lock) => lock,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let read = self.published();
        let Some(snapshot) = read.snapshot else {
            return Ok(None);
        };
        let Some(state) = read
            .status
            .state
            .as_ref()
            .filter(|state| state.rev == snapshot.rev)
        else {
            return Ok(None);
        };
        let content = super::memo::get(&self.snapshot_path())
            .map_or_else(|| snapshot.content(), |held| held.content());
        let content = aterm_codec::hex::encode_lower(&content);
        if state
            .snapshot
            .as_ref()
            .is_some_and(|known| known != &content)
        {
            return Ok(None);
        }
        let Some(bytes) = self.baseline(snapshot.rev) else {
            return Ok(None);
        };
        let digest = self.baseline_digest(snapshot.rev, &bytes);
        Ok(Some((
            snapshot,
            read.status,
            format!(".{digest}+{content}"),
        )))
    }
}

#[cfg(any(unix, windows))]
mod write {
    use super::*;
    use crate::{
        Rendered,
        file::{
            BASELINES,
            write::{private_dir, put},
        },
    };
    use std::{
        collections::BTreeSet,
        io::{BufRead as _, Read as _},
        path::Path,
    };

    /// New row-wise baselines declare their whole header and carry their own displayed cells.
    /// Earlier grids may need the snapshot at their revision to reconstruct a TSV's columns.
    /// Only a bounded header is read when a revision advances, irrespective of its row count.
    fn needs_render(path: &Path) -> io::Result<bool> {
        let file = match aterm_uds::regular_file::open(path) {
            Ok((file, _)) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        let mut head = Vec::new();
        io::BufReader::new(file.take(64 * 1024)).read_until(b'\n', &mut head)?;
        if head.first() != Some(&b'{') {
            return Ok(false);
        }
        Ok(!aterm_json::from_slice::<aterm_json::Value>(&head)
            .ok()
            .is_some_and(|value| {
                value.get("whole").is_some()
                    || value.get("format").and_then(aterm_json::Value::as_str)
                        == Some("aterm-file-base/1")
            }))
    }

    impl File {
        /// Preserve a rendering only for an old baseline that cannot stand alone. The caller
        /// still holds the recording lock and has not replaced the prior snapshot or baseline.
        fn save_render(&self, held: &Rendered, previous: Option<&[u8]>) -> io::Result<()> {
            let rev = held.snapshot().rev;
            if !needs_render(&self.root.baseline(&self.name, rev))? {
                return Ok(());
            }
            let content = aterm_codec::hex::encode_lower(&held.content());
            if !self.state().is_some_and(|state| {
                state.rev == rev
                    && state
                        .snapshot
                        .as_ref()
                        .is_none_or(|known| known == &content)
            }) {
                return Ok(());
            }
            let bytes;
            let grid = match previous {
                Some(bytes) => bytes,
                None => {
                    bytes = self.baseline(rev);
                    let Some(grid) = bytes.as_deref() else {
                        return Ok(());
                    };
                    grid
                }
            };
            let path = self.retained_render(rev);
            if !path.try_exists()? {
                put(&path, &held.encode())?;
            }
            let receipt = format!("{} {content}", self.baseline_digest(rev, grid));
            let path = self.render_receipt(rev);
            if read_bounded(&path, 129).ok().flatten().as_deref() != Some(receipt.as_bytes()) {
                put(&path, receipt.as_bytes())?;
            }
            Ok(())
        }

        pub(in crate::file) fn write_baseline(
            &self,
            rev: u64,
            bytes: &[u8],
            held: Option<&Rendered>,
        ) -> io::Result<bool> {
            let dir = self.root.baselines(&self.name);
            private_dir(&dir)?;
            let path = self.root.baseline(&self.name, rev);
            let previous = self.baseline(rev);
            if previous.as_deref() == Some(bytes) {
                return Ok(false);
            }
            if let Some(old) = held {
                self.save_render(
                    old,
                    if old.snapshot().rev == rev {
                        previous.as_deref()
                    } else {
                        None
                    },
                )?;
            }
            // Revisions order displayed contents. A format upgrade or new operation can
            // replace the underlying candidates at the very same displayed revision.
            let newest_alternate = if let Some(previous) = previous {
                let alternate = self.alternate_baseline(rev, &self.baseline_digest(rev, &previous));
                if !alternate.try_exists()? {
                    put(&alternate, &previous)?;
                }
                Some(alternate)
            } else {
                None
            };
            put(&path, bytes)?;
            prune(&dir, newest_alternate.as_deref())?;
            Ok(true)
        }
    }

    /// The latest eight numbered baselines, plus at most eight exact alternatives in total.
    /// A legacy rendering ages out with its numbered revision. Other files are left alone.
    fn prune(dir: &Path, newest: Option<&Path>) -> io::Result<()> {
        let mut numbered = BTreeSet::new();
        let mut alternatives = Vec::new();
        let mut renders = Vec::new();
        for item in std::fs::read_dir(dir)? {
            let item = item?;
            let os_name = item.file_name();
            let Some((rev, suffix)) = os_name.to_str().and_then(|name| name.split_once('.')) else {
                continue;
            };
            let Ok(rev) = rev.parse::<u64>() else {
                continue;
            };
            match suffix {
                "json" => {
                    numbered.insert(rev);
                }
                "render" | "receipt" => renders.push((rev, item.path())),
                _ if suffix
                    .strip_suffix(".base")
                    .is_some_and(|digest| aterm_codec::hex::is_lower(digest, 64)) =>
                {
                    let path = item.path();
                    alternatives.push((
                        rev,
                        Some(path.as_path()) == newest,
                        item.metadata()?.modified()?,
                        path,
                    ));
                }
                _ => {}
            }
        }
        let latest: BTreeSet<_> = numbered.iter().rev().take(BASELINES).copied().collect();
        alternatives.sort_unstable_by(|left, right| right.cmp(left));
        let mut kept = 0;
        for (rev, _, _, path) in alternatives {
            if latest.contains(&rev) && kept < BASELINES {
                kept += 1;
            } else {
                std::fs::remove_file(path)?;
            }
        }
        for (rev, path) in renders {
            if !latest.contains(&rev) {
                std::fs::remove_file(path)?;
            }
        }
        for rev in numbered.difference(&latest) {
            std::fs::remove_file(dir.join(format!("{rev}.json")))?;
        }
        Ok(())
    }
}
