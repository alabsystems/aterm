// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A file tree's status as one digest: what a walk that reads no file compares, to tell whether
//! anything a content digest of the same tree read could have changed since.
//!
//! Each entry the walk visits counts by its own status ([`StatKey`]: its device and inode, its
//! type and permission bits, its size, and its modification and status-change times in
//! nanoseconds); a directory adds its names, and a link its target text and the status of what it
//! resolves to. A write moves the modification time. Setting that time back, a chmod, a new or
//! removed hard link, an extended attribute and a rename onto the entry each move the
//! status-change time, which no process can set; a replacement is another inode.
//!
//! A time moves only to the next tick of the clock that stamps it, and that clock can be coarse:
//! APFS keeps nanoseconds, HFS+ whole seconds, and Linux before 6.13 stamps an inode from a
//! jiffy clock, so there a write a few milliseconds after the last change can leave both times as
//! they were. So two equal digests of a Unix tree, the second taken while nothing it names is
//! being written, mean no byte, mode or name below it changed between them when the first walk
//! began at least [`SETTLE_NS`] after the newest status change it recorded
//! ([`StatDigest::newest_change_ns`]): any later write is then stamped a later tick. A volume with
//! no status-change time of its own (FAT's is its modification time) has no such guarantee. The
//! walk is the caller's: this module only encodes what it visits, in the order it visits it (the
//! release's build-inputs seal records one beside its content digest, in the same walk).

/// How long before a walk begins every status it records must have last changed, in
/// nanoseconds, for its digest to tell any later write ([`StatDigest::newest_change_ns`]): twice
/// the coarsest tick among the clocks that stamp a real status-change time (HFS+'s whole second),
/// for a clock that steps.
pub const SETTLE_NS: i128 = 2_000_000_000;

/// One entry's status, as [`StatDigest`] records it and a read of a file is held to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatKey {
    /// The device the inode is on.
    pub dev: u64,
    /// The inode.
    pub ino: u64,
    /// The file type and permission bits (`st_mode`).
    pub mode: u32,
    /// The size in bytes.
    pub size: u64,
    /// The modification time, in nanoseconds since the Unix epoch.
    pub mtime_ns: i128,
    /// The status-change time, in nanoseconds since the Unix epoch.
    pub ctime_ns: i128,
}

impl StatKey {
    /// `meta`'s status.
    #[cfg(unix)]
    #[must_use]
    pub fn of(meta: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt as _;
        let ns = |secs: i64, nanos: i64| i128::from(secs) * 1_000_000_000 + i128::from(nanos);
        Self {
            dev: meta.dev(),
            ino: meta.ino(),
            mode: meta.mode(),
            size: meta.size(),
            mtime_ns: ns(meta.mtime(), meta.mtime_nsec()),
            ctime_ns: ns(meta.ctime(), meta.ctime_nsec()),
        }
    }

    /// `meta`'s status as far as std reads it off Unix: its size, its modification time and its
    /// read-only bit; the device, the inode and the status-change time are 0.
    #[cfg(not(unix))]
    #[must_use]
    pub fn of(meta: &std::fs::Metadata) -> Self {
        let mtime_ns = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| i128::try_from(d.as_nanos()).unwrap_or(i128::MAX));
        Self {
            dev: 0,
            ino: 0,
            mode: u32::from(meta.permissions().readonly()),
            size: meta.len(),
            mtime_ns,
            ctime_ns: 0,
        }
    }

    /// Whether `after` is still this status: the same inode on the same device, with the same
    /// size, modification time and status-change time. A file read for a digest is held to the
    /// status taken before the read and its descriptor's status after it; any difference means
    /// the bytes read may be no one version of the file.
    #[must_use]
    pub fn unchanged(&self, after: &Self) -> bool {
        (self.dev, self.ino, self.size, self.mtime_ns, self.ctime_ns)
            == (
                after.dev,
                after.ino,
                after.size,
                after.mtime_ns,
                after.ctime_ns,
            )
    }

    fn bytes(&self) -> [u8; 60] {
        let mut out = [0; 60];
        out[..8].copy_from_slice(&self.dev.to_le_bytes());
        out[8..16].copy_from_slice(&self.ino.to_le_bytes());
        out[16..20].copy_from_slice(&self.mode.to_le_bytes());
        out[20..28].copy_from_slice(&self.size.to_le_bytes());
        out[28..44].copy_from_slice(&self.mtime_ns.to_le_bytes());
        out[44..60].copy_from_slice(&self.ctime_ns.to_le_bytes());
        out
    }
}

/// The status of one tree's entries, in the order its walk visits them, as one SHA-256. Every
/// record is tagged and every variable-length part carries its length, so no two different
/// walks encode alike.
pub struct StatDigest {
    sha: aterm_digest::Sha256,
    entries: u64,
    newest_change_ns: Option<i128>,
}

impl StatDigest {
    /// The digest of the tree labelled `label`.
    #[must_use]
    pub fn new(label: &[u8]) -> Self {
        let mut digest = Self {
            sha: aterm_digest::Sha256::new(),
            entries: 0,
            newest_change_ns: None,
        };
        digest.part(b"aterm-stat-digest-v1");
        digest.part(label);
        digest
    }

    /// An entry the walk visits at `relative` (its path below the root, empty for the root), by
    /// its own status: a link's own, not its target's.
    pub fn entry(&mut self, relative: &[u8], own: &StatKey) {
        self.entries += 1;
        self.sha.update(b"E");
        self.part(relative);
        self.record(own);
    }

    /// The link just entered: its target text, and the status of what it resolves to, `None`
    /// when it dangles.
    pub fn link(&mut self, target: &[u8], resolved: Option<&StatKey>) {
        self.sha.update(b"L");
        self.part(target);
        match resolved {
            Some(key) => {
                self.sha.update(b"R");
                self.record(key);
            }
            None => self.sha.update(b"D"),
        }
    }

    /// The link just entered leads back to a directory the walk is inside, and is not followed.
    pub fn ancestor(&mut self) {
        self.sha.update(b"A");
    }

    /// The directory just entered: its names, in the order the walk visits them.
    pub fn names<'a>(&mut self, names: impl IntoIterator<Item = &'a [u8]>) {
        self.sha.update(b"N");
        let mut count = 0u64;
        for name in names {
            count += 1;
            self.part(name);
        }
        self.sha.update(b"n");
        self.sha.update(count.to_le_bytes());
    }

    /// The newest status-change time among the statuses recorded so far (each entry's own and
    /// each link's resolved one), in nanoseconds since the Unix epoch; `None` before any. The
    /// digest tells every later write only when its walk began [`SETTLE_NS`] after this.
    #[must_use]
    pub const fn newest_change_ns(&self) -> Option<i128> {
        self.newest_change_ns
    }

    /// The digest, and how many entries it recorded.
    #[must_use]
    pub fn finish(self) -> ([u8; 32], u64) {
        (self.sha.finalize(), self.entries)
    }

    fn record(&mut self, key: &StatKey) {
        self.sha.update(key.bytes());
        self.newest_change_ns = self.newest_change_ns.max(Some(key.ctime_ns));
    }

    fn part(&mut self, bytes: &[u8]) {
        self.sha.update((bytes.len() as u64).to_le_bytes());
        self.sha.update(bytes);
    }
}

#[cfg(test)]
#[path = "stat_digest_tests.rs"]
mod tests;
