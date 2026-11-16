// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded observations of an owned installer fixture, shared with its regression test.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{self, Read as _};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const MAX_ENTRIES: usize = 512;
const MAX_BYTES: usize = 4 * 1024 * 1024;

pub fn seed(home: &Path) -> io::Result<()> {
    for (dir, file) in [(".claude", "CLAUDE.md"), (".codex", "AGENTS.md")] {
        let dir = home.join(dir);
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join(file), b"Fixture instructions.\n")?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Identity {
    pub device: u64,
    pub inode: u64,
}

#[derive(Debug)]
pub struct SavedFile {
    // Retaining the descriptor prevents inode reuse from disguising replacement.
    _file: File,
    pub bytes: Vec<u8>,
    pub identity: Identity,
    pub modified: SystemTime,
    pub allocated: u64,
}

#[derive(Debug)]
pub struct Tree {
    pub files: BTreeMap<PathBuf, SavedFile>,
    dirs: BTreeSet<PathBuf>,
    directory_allocated: u64,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Drift {
    pub added: usize,
    pub removed: usize,
    pub bytes: usize,
    pub identity: usize,
    pub modified: usize,
    pub directories: bool,
}

impl Tree {
    pub fn read(root: &Path) -> io::Result<Self> {
        let mut tree = Self {
            files: BTreeMap::new(),
            dirs: BTreeSet::new(),
            directory_allocated: 0,
        };
        let mut entries = 0;
        let mut room = MAX_BYTES;
        tree.walk(root, root, &mut entries, &mut room)?;
        Ok(tree)
    }

    fn walk(
        &mut self,
        root: &Path,
        path: &Path,
        entries: &mut usize,
        room: &mut usize,
    ) -> io::Result<()> {
        *entries += 1;
        if *entries > MAX_ENTRIES {
            return Err(io::Error::other("installer fixture exceeds entry bound"));
        }
        let metadata = std::fs::symlink_metadata(path)?;
        let relative = path.strip_prefix(root).expect("walk stays below its root");
        if metadata.is_dir() {
            self.dirs.insert(relative.to_path_buf());
            self.directory_allocated += metadata.blocks() * 512;
            for entry in std::fs::read_dir(path)? {
                self.walk(root, &entry?.path(), entries, room)?;
            }
        } else if metadata.is_file() {
            let file = File::open(path)?;
            let metadata = file.metadata()?;
            let mut bytes = Vec::new();
            (&file).take(*room as u64 + 1).read_to_end(&mut bytes)?;
            if bytes.len() > *room {
                return Err(io::Error::other("installer fixture exceeds byte bound"));
            }
            *room -= bytes.len();
            self.files.insert(
                relative.to_path_buf(),
                SavedFile {
                    _file: file,
                    bytes,
                    identity: Identity {
                        device: metadata.dev(),
                        inode: metadata.ino(),
                    },
                    modified: metadata.modified()?,
                    allocated: metadata.blocks() * 512,
                },
            );
        } else {
            return Err(io::Error::other(
                "installer fixture contains a non-regular entry",
            ));
        }
        Ok(())
    }

    pub fn drift(&self, after: &Self) -> Drift {
        let mut drift = Drift {
            added: after
                .files
                .keys()
                .filter(|p| !self.files.contains_key(*p))
                .count(),
            directories: self.dirs != after.dirs,
            ..Drift::default()
        };
        for (path, before) in &self.files {
            if let Some(after) = after.files.get(path) {
                drift.bytes += usize::from(before.bytes != after.bytes);
                drift.identity += usize::from(before.identity != after.identity);
                drift.modified += usize::from(before.modified != after.modified);
            } else {
                drift.removed += 1;
            }
        }
        drift
    }

    pub fn logical_bytes(&self) -> usize {
        self.files.values().map(|file| file.bytes.len()).sum()
    }

    /// Blocks of the reachable captured files and directories, not peak disk usage.
    /// Replaced inodes retained by older identity snapshots are excluded.
    pub fn allocated_bytes(&self) -> u64 {
        self.directory_allocated + self.files.values().map(|file| file.allocated).sum::<u64>()
    }

    pub fn directory_count(&self) -> usize {
        self.dirs.len()
    }

    /// A fixed past mtime makes an in-place write observable without sleeping.
    pub fn age_files(&self) -> io::Result<()> {
        let past = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        for file in self.files.values() {
            file._file
                .set_times(std::fs::FileTimes::new().set_modified(past))?;
        }
        Ok(())
    }
}
