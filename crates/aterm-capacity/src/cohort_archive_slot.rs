// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A stable archive mutation description outlives the claim directory it protects. Every
//! query keeps the original named slot and entry origins through its final result.

use super::*;
use std::collections::{BTreeMap, BTreeSet};

const GATE: &str = ".slot.lock";
const GATE_BODY: &str = "cohort archive gate v1\n";
type DirOrigin = (u64, u64, u32, i64, i64, i64, i64);

#[path = "cohort_archive_preparation.rs"]
mod preparation;

enum Entry {
    File { file: File, stamp: Stamp },
    Directory { file: File, id: Value },
}

pub(super) struct Slot {
    parent: File,
    key: String,
    directory: File,
    id: Value,
    origin: DirOrigin,
    gate: RecordLock,
    gate_stamp: Stamp,
    entries: BTreeMap<String, Entry>,
}

/// A query may inspect frozen metadata under the fresh gate without covering or mutating
/// anything for a foreign request. Only consuming the same observation covers its gate.
pub(super) struct Inspection {
    slot: Slot,
}
impl Inspection {
    pub(super) fn read(&self, name: &str) -> io::Result<(File, String, Stamp)> {
        self.slot.read(name)
    }
    pub(super) fn identity(&self) -> &Value {
        self.slot.identity()
    }
    pub(super) fn gate_stamp(&self) -> &Stamp {
        self.slot.gate_stamp()
    }
    pub(super) fn history(&self) -> io::Result<Option<File>> {
        self.slot.history()
    }
    pub(super) fn check(&self) -> io::Result<()> {
        self.slot.check()
    }
    pub(super) fn cover(self) -> io::Result<Slot> {
        self.slot.cover_gate()?;
        Ok(self.slot)
    }
    #[cfg(test)]
    pub(super) fn observed(&self) -> &Slot {
        &self.slot
    }
}

fn busy(error: std::fs::TryLockError) -> io::Error {
    match error {
        std::fs::TryLockError::WouldBlock => {
            io::Error::new(io::ErrorKind::WouldBlock, "cohort archive slot is busy")
        }
        std::fs::TryLockError::Error(error) => error,
    }
}

impl Slot {
    pub(super) fn prepare(parent: &File, key: &str) -> io::Result<Self> {
        let prepared = match fd::fstatat(parent, key, fd::AtFlags::SYMLINK_NOFOLLOW) {
            Ok(_) => None,
            Err(error) if error.is_noent() => Some(preparation::publish(parent, key)?),
            Err(error) => return Err(error.into()),
        };
        let slot = Self::capture(parent, key, prepared.as_ref())?;
        slot.cover_gate()?;
        if let Some(prepared) = &prepared {
            prepared.recheck(parent, key)?;
        }
        Ok(slot)
    }

    #[cfg(test)]
    pub(super) fn open(parent: &File, key: &str) -> io::Result<Self> {
        let slot = Self::capture(parent, key, None)?;
        slot.cover_gate()?;
        Ok(slot)
    }

    pub(super) fn inspect(parent: &File, key: &str) -> io::Result<Inspection> {
        Ok(Inspection {
            slot: Self::capture(parent, key, None)?,
        })
    }

    fn capture(
        parent: &File,
        key: &str,
        prepared: Option<&preparation::Published>,
    ) -> io::Result<Self> {
        if let Some(prepared) = &prepared {
            prepared.recheck(parent, key)?
        }
        let directory = open(parent, key, false, true)?;
        let id = identity(&directory, true)?;
        let (file, body, gate_stamp) = held_immutable(&directory, GATE)?;
        if prepared
            .as_ref()
            .is_some_and(|prepared| prepared.stamp != gate_stamp)
        {
            return Err(bad(
                "archive slot reopened a different gate from its original prepared file",
            ));
        }
        if body != GATE_BODY {
            return Err(bad("archive slot has an unknown stable gate"));
        }
        // FRESH open description: aliases in this process must contend as well.
        let gate = RecordLock::try_lock(file, 1).map_err(busy)?;
        if Stamp::read(&gate)? != gate_stamp
            || Stamp::read(&open(&directory, GATE, false, false)?)? != gate_stamp
        {
            return Err(bad(
                "archive slot gate changed while acquiring its mutation description",
            ));
        }
        let origin = directory_origin(&directory)?;
        let mut slot = Self {
            parent: parent.try_clone()?,
            key: key.into(),
            directory,
            id,
            origin,
            gate,
            gate_stamp,
            entries: BTreeMap::new(),
        };
        for item in fd::Dir::read_from(slot.directory.as_fd())? {
            let item = item?;
            let name = item
                .file_name()
                .to_str()
                .map_err(|_| bad("non-UTF8 archive slot child"))?;
            if matches!(name, "." | "..") {
                continue;
            }
            if slot.entries.len() >= 4 {
                return Err(bad("unbounded archive slot entries"));
            }
            let entry = if matches!(name, GATE | RECEIPT | COMPACTED) {
                let file = open(&slot.directory, name, false, false)?;
                if file.metadata()?.mode() & 0o7777 != 0o400 {
                    return Err(bad("archive metadata is not sealed read-only"));
                }
                Entry::File {
                    stamp: Stamp::read(&file)?,
                    file,
                }
            } else if name == HISTORY {
                let file = open(&slot.directory, name, false, true)?;
                Entry::Directory {
                    id: identity(&file, true)?,
                    file,
                }
            } else {
                return Err(bad("unknown archive slot data is preserved"));
            };
            slot.entries.insert(name.into(), entry);
        }
        slot.check()?;
        if let Some(prepared) = &prepared {
            prepared.recheck(parent, key)?
        }
        Ok(slot)
    }

    fn cover_gate(&self) -> io::Result<()> {
        self.check()?;
        cover_frozen(
            &self.directory,
            GATE,
            &self.gate,
            GATE_BODY,
            &self.gate_stamp,
        )?;
        self.sync()
    }

    pub(super) fn file(&self) -> &File {
        &self.directory
    }
    pub(super) fn identity(&self) -> &Value {
        &self.id
    }
    pub(super) fn gate_stamp(&self) -> &Stamp {
        &self.gate_stamp
    }

    pub(super) fn history(&self) -> io::Result<Option<File>> {
        self.check()?;
        match self.entries.get(HISTORY) {
            Some(Entry::Directory { file, .. }) => Ok(Some(file.try_clone()?)),
            None => Ok(None),
            _ => Err(bad("archive history is not a retained directory")),
        }
    }

    pub(super) fn check(&self) -> io::Result<()> {
        self.check_entries()?;
        if directory_origin(&self.directory)? != self.origin {
            return Err(bad("archive slot changed from its frozen directory origin"));
        }
        Ok(())
    }

    fn check_entries(&self) -> io::Result<()> {
        if identity(&open(&self.parent, &self.key, false, true)?, true)? != self.id
            || identity(&self.directory, true)? != self.id
            || Stamp::read(&self.gate)? != self.gate_stamp
            || Stamp::read(&open(&self.directory, GATE, false, false)?)? != self.gate_stamp
        {
            return Err(bad(
                "archive slot or stable gate is detached, replaced or restored",
            ));
        }
        let mut names = BTreeSet::new();
        for item in fd::Dir::read_from(self.directory.as_fd())? {
            let item = item?;
            let name = item
                .file_name()
                .to_str()
                .map_err(|_| bad("non-UTF8 archive slot entry"))?;
            if matches!(name, "." | "..") {
                continue;
            }
            if names.len() >= 4 || !self.entries.contains_key(name) {
                return Err(bad("archive slot gained unknown data"));
            }
            names.insert(name.to_owned());
        }
        if names.len() != self.entries.len() {
            return Err(bad("archive slot lost an original entry"));
        }
        for (name, entry) in &self.entries {
            match entry {
                Entry::File { file, stamp } => {
                    if Stamp::read(file)? != *stamp
                        || Stamp::read(&open(&self.directory, name, false, false)?)? != *stamp
                    {
                        return Err(bad(
                            "archive metadata changed from its original file origin",
                        ));
                    }
                }
                Entry::Directory { file, id } => {
                    if identity(file, true)? != *id
                        || identity(&open(&self.directory, name, false, true)?, true)? != *id
                    {
                        return Err(bad("archive history directory changed identity"));
                    }
                }
            }
        }
        Ok(())
    }

    pub(super) fn read(&self, name: &str) -> io::Result<(File, String, Stamp)> {
        self.check()?;
        let Entry::File { file, stamp } = self.entries.get(name).ok_or(io::ErrorKind::NotFound)?
        else {
            return Err(bad("archive entry is not a metadata file"));
        };
        let (text, actual) = bytes(file, MAX_ENTRY)?;
        if actual != *stamp {
            return Err(bad("archive read changed its original metadata origin"));
        }
        self.check()?;
        Ok((file.try_clone()?, text, stamp.clone()))
    }

    pub(super) fn cover(
        &self,
        name: &str,
        file: &File,
        text: &str,
        stamp: &Stamp,
    ) -> io::Result<()> {
        self.check()?;
        let Entry::File {
            stamp: original, ..
        } = self.entries.get(name).ok_or(io::ErrorKind::NotFound)?
        else {
            return Err(bad("archive cover is not a file"));
        };
        if original != stamp {
            return Err(bad("archive cover differs from its parsed original file"));
        }
        cover_frozen(&self.directory, name, file, text, stamp)?;
        self.check()
    }

    pub(super) fn sealed(&mut self, name: &str, text: &str) -> io::Result<(File, Stamp)> {
        self.check()?;
        if !matches!(name, RECEIPT | COMPACTED) || text.len() > MAX_ENTRY {
            return Err(bad("unknown or oversized archive publication"));
        }
        if self.entries.contains_key(name) {
            let (file, actual, stamp) = self.read(name)?;
            if actual != text {
                return Err(bad(
                    "archive publication differs from its immutable first intent",
                ));
            }
            self.cover(name, &file, text, &stamp)?;
            return Ok((file, stamp));
        }
        let mut file = open(&self.directory, name, true, false)?;
        let origin = directory_origin(&self.directory)?;
        file.write_all(text.as_bytes())?;
        fd::fchmod(&file, Mode::RUSR)?;
        let stamp = Stamp::read(&file)?;
        self.entries.insert(
            name.into(),
            Entry::File {
                file: file.try_clone()?,
                stamp: stamp.clone(),
            },
        );
        self.origin = origin;
        self.check()?;
        self.cover(name, &file, text, &stamp)?;
        Ok((file, stamp))
    }

    pub(super) fn move_claim(
        &mut self,
        parent: &File,
        key: &str,
        claim: &File,
    ) -> io::Result<DirOrigin> {
        self.check()?;
        if self.entries.contains_key(HISTORY) {
            return Err(bad("archive already contains a history"));
        }
        let id = identity(claim, true)?;
        if identity(&open(parent, key, false, true)?, true)? != id {
            return Err(bad("claim changed before its known archive move"));
        }
        fd::renameat_with(
            parent,
            key,
            &self.directory,
            HISTORY,
            RenameFlags::NOREPLACE,
        )?;
        // These are the immediate effects of this exact rename, before either parent flush.
        let origin = directory_origin(&self.directory)?;
        let claim_origin = directory_origin(claim)?;
        self.entries.insert(
            HISTORY.into(),
            Entry::Directory {
                file: claim.try_clone()?,
                id,
            },
        );
        self.origin = origin;
        self.check()?;
        Ok(claim_origin)
    }

    pub(super) fn remove_history(&mut self, expected: &Value) -> io::Result<()> {
        self.check()?;
        let Some(Entry::Directory { id, .. }) = self.entries.get(HISTORY) else {
            return Err(bad("archive has no held history to remove"));
        };
        if id != expected {
            return Err(bad("archive history differs from its committed inventory"));
        }
        fd::unlinkat(&self.directory, HISTORY, fd::AtFlags::REMOVEDIR)?;
        let origin = directory_origin(&self.directory)?;
        self.entries.remove(HISTORY);
        self.origin = origin;
        self.check()?;
        self.sync()
    }

    pub(super) fn sync(&self) -> io::Result<()> {
        self.check()?;
        aterm_durable::durable(&self.directory)?;
        self.check()?;
        aterm_durable::durable(&self.parent)?;
        self.check()
    }
}

#[cfg(test)]
pub(super) fn interrupt_preparation(stage: &'static str) {
    preparation::interrupt(stage)
}
