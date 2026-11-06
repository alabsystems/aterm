// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Durable, create-only generations for a cohort record. The writable record is never its
//! own authority: a generation binds its complete bytes and physical origin before the flush
//! it covers. A fresh open description serializes writer aliases, readers and retirement.
//! Incomplete generations and orphan metadata block; neither is an empty description.

use super::super::{
    CLAIM, ClaimGuardIdentity, CohortClaimState, RECORD_MAX, RecordLock, TAKING, WAIT,
};
use crate::cohort_journal::Journal;
use aterm_dirfd::{self as fd, AsFd as _, FileType, Mode, OFlags, RenameFlags};
use aterm_json::Value;
use std::fs::File;
use std::io::{self, Write as _};
use std::os::unix::fs::{FileExt as _, MetadataExt as _};

#[path = "cohort_epoch.rs"]
mod epoch;
use epoch::Epoch;
#[path = "cohort_archive.rs"]
mod archive;

#[cfg(test)]
#[path = "cohort_history_model_tests.rs"]
mod history_model_tests;

const HEADER: &str = "anchor.json";
const MUTATION: &str = "mutation.lock";
const MAX_ENTRY: usize = RECORD_MAX as usize * 8 + 4096;

fn bad(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why)
}

/// All mutable metadata relevant to the origin of these private regular files. Atime is
/// deliberately absent: reading a claim must not invalidate it.
#[derive(Clone, Debug, PartialEq)]
struct Stamp(Vec<Value>);

impl Stamp {
    fn read(file: &File) -> io::Result<Self> {
        let m = file.metadata()?;
        // SAFETY: geteuid reads the current process's effective identity.
        let uid = unsafe { libc::geteuid() };
        if !m.is_file() || m.nlink() != 1 || m.uid() != uid || m.mode() & 0o077 != 0 {
            return Err(bad("checkpoint file is not a private owned regular file"));
        }
        Ok(Self(vec![
            Value::from(m.dev()),
            Value::from(m.ino()),
            Value::from(m.len()),
            Value::from(u64::from(m.mode())),
            Value::from(m.nlink()),
            Value::from(u64::from(m.uid())),
            Value::from(m.mtime()),
            Value::from(m.mtime_nsec()),
            Value::from(m.ctime()),
            Value::from(m.ctime_nsec()),
        ]))
    }

    fn mode(&self) -> u32 {
        u32::try_from(self.0[3].as_u64().unwrap_or(0)).unwrap_or(0)
    }

    fn value(&self) -> Value {
        Value::Array(self.0.clone())
    }

    fn decode(v: &Value) -> io::Result<Self> {
        let a = v
            .as_array()
            .ok_or_else(|| bad("missing checkpoint origin"))?;
        if a.len() != 10
            || a.iter()
                .any(|n| n.as_u64().is_none() && n.as_i64().is_none())
        {
            return Err(bad("invalid checkpoint origin"));
        }
        Ok(Self(a.clone()))
    }
}

fn identity(file: &File, directory: bool) -> io::Result<Value> {
    let s = fd::fstat(file)?;
    // SAFETY: geteuid reads the current process's effective identity.
    let uid = unsafe { libc::geteuid() };
    if s.st_uid != uid
        || s.st_mode & 0o077 != 0
        || (directory && !FileType::from_raw_mode(s.st_mode).is_dir())
        || (!directory && (!FileType::from_raw_mode(s.st_mode).is_file() || s.st_nlink != 1))
    {
        return Err(bad("checkpoint anchor is not a private owned entry"));
    }
    Ok(aterm_json::json!([s.st_dev, s.st_ino, s.st_mode, s.st_uid]))
}

fn open(parent: &File, name: &str, create: bool, directory: bool) -> io::Result<File> {
    let mut flags = OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
    flags |= if create {
        OFlags::RDWR | OFlags::CREATE | OFlags::EXCL
    } else {
        OFlags::RDONLY
    };
    if directory {
        flags |= OFlags::DIRECTORY;
    }
    let file = aterm_uds::kept_out(|| {
        fd::openat(parent, name, flags, Mode::RUSR | Mode::WUSR)
            .map(File::from)
            .map_err(io::Error::from)
    })?;
    identity(&file, directory)?;
    Ok(file)
}

fn bytes(file: &File, limit: usize) -> io::Result<(String, Stamp)> {
    let before = Stamp::read(file)?;
    let before_len =
        usize::try_from(before.0[2].as_u64().unwrap_or(u64::MAX)).unwrap_or(usize::MAX);
    if before_len > limit {
        return Err(bad(
            "checkpoint bounded read changed origin or exceeded its bound",
        ));
    }
    let mut data = vec![0u8; before_len + 1];
    let mut n = 0;
    while n < data.len() {
        let got = file.read_at(&mut data[n..], n as u64)?;
        if got == 0 {
            break;
        }
        n += got;
    }
    if n > before_len || Stamp::read(file)? != before {
        return Err(bad(
            "checkpoint bounded read changed origin or exceeded its bound",
        ));
    }
    data.truncate(n);
    Ok((
        String::from_utf8(data).map_err(|_| bad("checkpoint is not UTF-8"))?,
        before,
    ))
}

fn immutable(parent: &File, name: &str) -> io::Result<(String, Stamp)> {
    let (_, text, stamp) = held_immutable(parent, name)?;
    Ok((text, stamp))
}

fn held_immutable(parent: &File, name: &str) -> io::Result<(File, String, Stamp)> {
    let file = open(parent, name, false, false)?;
    if file.metadata()?.mode() & 0o7777 != 0o400 {
        return Err(bad("checkpoint publication is not sealed read-only"));
    }
    let (text, stamp) = bytes(&file, MAX_ENTRY)?;
    Ok((file, text, stamp))
}

/// Cover the SAME descriptor and stamp from the semantic read. Cold header/tip readers
/// cannot reopen and capture a newer file after accepting its earlier bytes.
fn cover_frozen(
    parent: &File,
    name: &str,
    file: &File,
    expected: &str,
    before: &Stamp,
) -> io::Result<()> {
    let (text, stamp) = bytes(file, MAX_ENTRY)?;
    if text != expected
        || stamp != *before
        || Stamp::read(&open(parent, name, false, false)?)? != *before
    {
        return Err(bad("checkpoint metadata lost its frozen pre-flush origin"));
    }
    #[cfg(test)]
    if FAIL_PUBLICATION_FLUSH.with(|f| f.replace(false)) {
        return Err(io::Error::other("injected checkpoint FILE flush refusal"));
    }
    #[cfg(test)]
    if FAIL_NAMED_FILE_FLUSH.with(|slot| {
        if slot.get() == Some(name) {
            slot.set(None);
            true
        } else {
            false
        }
    }) {
        return Err(io::Error::other(
            "injected exact metadata FILE flush refusal",
        ));
    }
    #[cfg(test)]
    if FAIL_EXACT_FILE_FLUSH.with(|slot| {
        let identity = (
            before.0[0].as_u64().expect("checked device"),
            before.0[1].as_u64().expect("checked inode"),
        );
        if slot.get() == Some(identity) {
            slot.set(None);
            true
        } else {
            false
        }
    }) {
        return Err(io::Error::other(
            "injected original metadata descriptor FILE flush refusal",
        ));
    }
    #[cfg(test)]
    if name.ends_with(".pending") && FAIL_PENDING_FILE_FLUSH.with(|f| f.replace(false)) {
        return Err(io::Error::other(
            "injected pending checkpoint FILE flush refusal",
        ));
    }
    aterm_durable::durable(file)?;
    #[cfg(test)]
    archive::after_metadata_cover(parent, name, "file")?;
    let (text, stamp) = bytes(file, MAX_ENTRY)?;
    if text != expected
        || stamp != *before
        || Stamp::read(&open(parent, name, false, false)?)? != *before
    {
        return Err(bad(
            "checkpoint metadata changed during its covering file flush",
        ));
    }
    aterm_durable::durable(parent)?;
    #[cfg(test)]
    archive::after_metadata_cover(parent, name, "directory")?;
    #[cfg(test)]
    if name == "base.json" {
        epoch::after_original_base_cover(parent)?;
    }
    if Stamp::read(file)? != *before || Stamp::read(&open(parent, name, false, false)?)? != *before
    {
        return Err(bad(
            "checkpoint metadata changed during its covering directory flush",
        ));
    }
    Ok(())
}

fn stem(name: &str) -> io::Result<&str> {
    let stem = [CLAIM, WAIT, TAKING]
        .iter()
        .find_map(|suffix| name.strip_suffix(suffix))
        .ok_or_else(|| bad("checkpoint claim has no recognized suffix"))?;
    if stem.is_empty() || !stem.bytes().all(|b| b.is_ascii_digit() || b == b'-') {
        return Err(bad("checkpoint claim has an invalid stem"));
    }
    Ok(stem)
}

pub(super) fn metadata_name(name: &str) -> bool {
    crate::cohort_namespace::valid_claim_directory(std::ffi::OsStr::new(name))
}

#[derive(Debug)]
pub(super) struct Anchor {
    pub(super) directory: File,
    parent: File,
    name: String,
    stem: String,
    epoch: String,
    guard: ClaimGuardIdentity,
    header: String,
    header_stamp: Stamp,
}

impl Anchor {
    pub(super) fn create(backend: &File, claim: &str, nonce: &str) -> io::Result<(Self, File)> {
        let raw_stem = stem(claim)?.to_owned();
        let epoch = aterm_uds::rand::hex_token::<16>()?;
        let name = format!("{raw_stem}.{epoch}.noindex");
        fd::mkdirat(backend, name.as_str(), Mode::RWXU)?;
        let directory = open(backend, &name, false, true)?;
        let writer = open(&directory, claim, true, false)?;
        let stat = writer.metadata()?;
        let guard = ClaimGuardIdentity {
            device: stat.dev(),
            inode: stat.ino(),
        };
        let mut lock = open(&directory, MUTATION, true, false)?;
        lock.write_all(b"cohort mutation lock v1\n")?;
        aterm_durable::durable(&lock)?;
        let header = aterm_json::to_string(&aterm_json::json!({
            "v": 3u64, "namespace": nonce, "stem": raw_stem,"epoch":epoch,
            "parent": identity(backend, true)?, "directory": identity(&directory, true)?,
            "lock": identity(&lock, false)?,"guard":[guard.device,guard.inode],
        }))
        .map_err(|_| bad("cannot encode checkpoint anchor"))?;
        let mut file = open(&directory, HEADER, true, false)?;
        file.write_all(header.as_bytes())?;
        fd::fchmod(&file, Mode::RUSR)?;
        let header_stamp = Stamp::read(&file)?;
        cover_frozen(&directory, HEADER, &file, &header, &header_stamp)?;
        aterm_durable::durable(backend)?;
        Ok((
            Self {
                directory,
                parent: backend.try_clone()?,
                name,
                stem: raw_stem,
                epoch,
                guard,
                header,
                header_stamp,
            },
            writer,
        ))
    }

    pub(super) fn open(backend: &File, claim: &str, nonce: &str) -> io::Result<Self> {
        let raw_stem = stem(claim)?.to_owned();
        let directory = backend.try_clone()?;
        identity(&directory, true)?;
        let (header_file, header, header_stamp) = held_immutable(&directory, HEADER)?;
        let value: Value =
            aterm_json::from_str(&header).map_err(|_| bad("invalid claim anchor"))?;
        let epoch = value
            .get("epoch")
            .and_then(Value::as_str)
            .ok_or_else(|| bad("claim has no epoch"))?
            .to_owned();
        let original = value
            .get("guard")
            .and_then(Value::as_array)
            .filter(|a| a.len() == 2)
            .ok_or_else(|| bad("anchor has no original guard"))?;
        let guard = ClaimGuardIdentity {
            device: original[0]
                .as_u64()
                .ok_or_else(|| bad("invalid guard device"))?,
            inode: original[1]
                .as_u64()
                .filter(|n| *n > 0)
                .ok_or_else(|| bad("invalid guard inode"))?,
        };
        let name = format!("{raw_stem}.{epoch}.noindex");
        if !metadata_name(&name) {
            return Err(bad("claim has an invalid epoch"));
        }
        let parent = open(&directory, "..", false, true)?;
        let lock = open(&directory, MUTATION, false, false)?;
        let expected = aterm_json::to_string(&aterm_json::json!({
            "v": 3u64, "namespace": nonce, "stem": raw_stem,"epoch":epoch,
            "parent": identity(&parent, true)?, "directory": identity(&directory, true)?,
            "lock": identity(&lock, false)?,"guard":[guard.device,guard.inode],
        }))
        .map_err(|_| bad("cannot encode checkpoint anchor"))?;
        if header != expected {
            return Err(bad("checkpoint anchor was copied or replaced"));
        }
        cover_frozen(&directory, HEADER, &header_file, &header, &header_stamp)?;
        let anchor = Self {
            directory,
            parent,
            name,
            stem: raw_stem,
            epoch,
            guard,
            header,
            header_stamp,
        };
        anchor.validate(backend)?;
        Ok(anchor)
    }

    pub(super) fn open_name(parent: &File, name: &str, nonce: &str) -> io::Result<Self> {
        if !metadata_name(name) {
            return Err(bad("unknown active claim directory"));
        }
        let marker = name
            .split('.')
            .next()
            .ok_or_else(|| bad("claim has no marker"))?;
        let directory = open(parent, name, false, true)?;
        let anchor = Self::open(&directory, &format!("{marker}{CLAIM}"), nonce)?;
        if anchor.name != name || identity(parent, true)? != identity(&anchor.parent, true)? {
            return Err(bad("claim anchor is not at its original active name"));
        }
        anchor.validate(&directory)?;
        Ok(anchor)
    }

    pub(super) fn key(&self) -> &str {
        &self.name
    }

    pub(super) fn matches_guard(&self, guard: ClaimGuardIdentity) -> bool {
        self.guard == guard
    }

    pub(super) fn closure_ref(
        &self,
        namespace: &str,
        guard: ClaimGuardIdentity,
    ) -> io::Result<crate::CohortClosureRef> {
        if !self.matches_guard(guard) {
            return Err(bad("held guard differs from immutable claim origin"));
        }
        crate::CohortClosureRef::captured(namespace, &self.stem, &self.epoch, guard)
    }

    pub(super) fn lock(&self, backend: &File) -> io::Result<Transaction> {
        self.lock_inner(backend, false)
    }

    pub(super) fn lock_recover(&self, backend: &File) -> io::Result<Transaction> {
        self.lock_inner(backend, true)
    }

    fn lock_inner(&self, backend: &File, recover: bool) -> io::Result<Transaction> {
        self.validate(backend)?;
        // A FRESH open, never a dup of the guard or a transferred controller, arbitrates
        // aliases in the same process as well as other processes. Busy Drop preserves.
        let lock = open(&self.directory, MUTATION, false, false)?;
        let lock = RecordLock::try_lock(lock, 1).map_err(|e| match e {
            std::fs::TryLockError::WouldBlock => {
                io::Error::new(io::ErrorKind::WouldBlock, "cohort mutation is busy")
            }
            std::fs::TryLockError::Error(e) => e,
        })?;
        self.validate(backend)?;
        let anchor = Self {
            directory: self.directory.try_clone()?,
            parent: self.parent.try_clone()?,
            name: self.name.clone(),
            stem: self.stem.clone(),
            epoch: self.epoch.clone(),
            guard: self.guard,
            header: self.header.clone(),
            header_stamp: self.header_stamp.clone(),
        };
        let mut transaction = Transaction {
            anchor,
            _lock: lock,
            latest: None,
            pending: None,
            tip_file: None,
            pending_file: None,
            epoch: None,
        };
        transaction.load(recover)?;
        transaction.anchor.validate(backend)?;
        Ok(transaction)
    }

    fn validate(&self, backend: &File) -> io::Result<()> {
        let named = open(&self.parent, &self.name, false, true)?;
        let dir_id = identity(&self.directory, true)?;
        if identity(backend, true)? != dir_id || identity(&named, true)? != dir_id {
            return Err(bad("checkpoint directory was replaced"));
        }
        let (header, stamp) = immutable(&self.directory, HEADER)?;
        if header != self.header || stamp != self.header_stamp {
            return Err(bad("checkpoint header changed or was restored"));
        }
        let lock = open(&self.directory, MUTATION, false, false)?;
        let expected: Value =
            aterm_json::from_str(&self.header).map_err(|_| bad("invalid anchor"))?;
        if expected.get("lock") != Some(&identity(&lock, false)?) {
            return Err(bad("checkpoint mutation lock was replaced"));
        }
        let mut present = 0;
        for suffix in [TAKING, WAIT, CLAIM] {
            let name = format!("{}{suffix}", self.stem);
            match fd::fstatat(&self.directory, name, fd::AtFlags::SYMLINK_NOFOLLOW) {
                Ok(stat) => {
                    present += 1;
                    if present > 1
                        || !FileType::from_raw_mode(stat.st_mode).is_file()
                        || (stat.st_dev, stat.st_ino) != (self.guard.device, self.guard.inode)
                    {
                        return Err(bad(
                            "working record differs from its immutable original guard",
                        ));
                    }
                }
                Err(error) if error.is_noent() => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Capture {
    ordinal: u64,
    name: String,
    text: String,
    origin: Stamp,
    previous: Option<Stamp>,
    retired: bool,
}

impl Capture {
    fn encode(&self) -> io::Result<String> {
        aterm_json::to_string(&aterm_json::json!({
            "v": 1u64, "ordinal": self.ordinal, "name": self.name, "text": self.text,
            "origin": self.origin.value(), "previous": self.previous.as_ref().map(Stamp::value),
            "retired": self.retired,
        }))
        .map_err(|_| bad("cannot encode cohort generation"))
    }

    fn decode(text: &str) -> io::Result<Self> {
        let v: Value = aterm_json::from_str(text).map_err(|_| bad("torn cohort generation"))?;
        let get = |k| {
            v.get(k)
                .and_then(Value::as_str)
                .ok_or_else(|| bad("missing generation text"))
        };
        if v.as_object().is_none_or(|o| o.len() != 7)
            || v.get("v").and_then(Value::as_u64) != Some(1)
        {
            return Err(bad("unknown cohort generation"));
        }
        let capture = Self {
            ordinal: v
                .get("ordinal")
                .and_then(Value::as_u64)
                .ok_or_else(|| bad("missing generation ordinal"))?,
            name: get("name")?.to_owned(),
            text: get("text")?.to_owned(),
            origin: Stamp::decode(v.get("origin").ok_or_else(|| bad("missing origin"))?)?,
            previous: match v.get("previous") {
                Some(Value::Null) => None,
                Some(s) => Some(Stamp::decode(s)?),
                None => return Err(bad("missing predecessor")),
            },
            retired: v
                .get("retired")
                .and_then(Value::as_bool)
                .ok_or_else(|| bad("missing retirement state"))?,
        };
        stem(&capture.name)?;
        if capture.text.len() as u64 > RECORD_MAX || capture.encode()? != text {
            return Err(bad("noncanonical cohort generation"));
        }
        Ok(capture)
    }

    pub(super) fn check(&self, backend: &File, writer: &File) -> io::Result<()> {
        let (text, origin) = bytes(writer, RECORD_MAX as usize)?;
        let named = open(backend, &self.name, false, false)?;
        if text != self.text || origin != self.origin || Stamp::read(&named)? != self.origin {
            return Err(bad("cohort generation changed or was restored"));
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(super) struct Transaction {
    anchor: Anchor,
    _lock: RecordLock,
    latest: Option<(Capture, Stamp)>,
    pending: Option<Capture>,
    /// Exact parsed descriptors are retained through authority/recovery, not reopened as a
    /// fresh origin after accepting earlier semantic bytes.
    tip_file: Option<File>,
    pending_file: Option<(File, Stamp)>,
    epoch: Option<Epoch>,
}

impl Transaction {
    fn load(&mut self, recover: bool) -> io::Result<()> {
        self.epoch = Epoch::open(&self.anchor.directory, &self.anchor.stem, recover)?;
        let Some(epoch) = &self.epoch else {
            return Ok(());
        };
        let directory = &epoch.directory;
        let before = directory_origin(directory)?;
        let (base_file, base_stamp) = epoch.base_descriptor()?;
        self.latest = Some((epoch.base.clone(), base_stamp));
        self.tip_file = Some(base_file);
        let mut entries = std::collections::BTreeMap::new();
        let mut tip: Option<(u64, File)> = None;
        let mut checked = Vec::new();
        for item in fd::Dir::read_from(directory.as_fd())? {
            let item = item?;
            let name = item
                .file_name()
                .to_str()
                .map_err(|_| bad("non-UTF8 checkpoint entry"))?;
            if matches!(name, "." | ".." | "base.json") {
                continue;
            }
            if entries.len() + 1 >= epoch::WINDOW {
                return Err(bad("cohort checkpoint epoch exceeds its finite window"));
            }
            let (number, pending) = if let Some(n) = name.strip_suffix(".json") {
                (n, false)
            } else if let Some(n) = name.strip_suffix(".pending") {
                (n, true)
            } else {
                return Err(bad("unknown checkpoint entry"));
            };
            let ordinal: u64 = number
                .parse()
                .map_err(|_| bad("invalid checkpoint ordinal"))?;
            if format!("{ordinal:016}") != number || ordinal == 0 || entries.contains_key(&ordinal)
            {
                return Err(bad("duplicate or invalid checkpoint ordinal"));
            }
            let (file, text, stamp) = held_immutable(directory, name)?;
            let capture = Capture::decode(&text)?;
            if capture.ordinal != ordinal || stem(&capture.name)? != self.anchor.stem {
                return Err(bad("checkpoint ordinal differs from its name"));
            }
            checked.push((name.to_owned(), file.try_clone()?, stamp.clone()));
            if pending {
                if self.pending_file.is_some() {
                    return Err(bad("multiple pending checkpoint files"));
                }
                self.pending_file = Some((file, stamp.clone()));
            } else if tip.as_ref().is_none_or(|(number, _)| *number < ordinal) {
                tip = Some((ordinal, file));
            }
            entries.insert(ordinal, (capture, stamp, pending));
        }
        let first = epoch
            .base
            .ordinal
            .checked_add(1)
            .ok_or_else(|| bad("generation ordinal exhausted"))?;
        for (ordinal, (number, (capture, stamp, pending))) in (first..).zip(entries) {
            if number != ordinal
                || self.pending.is_some()
                || self.latest.as_ref().is_some_and(|(c, _)| c.retired)
                || capture.previous.as_ref() != self.latest.as_ref().map(|(_, s)| s)
            {
                return Err(bad(
                    "gapped, restored or post-retirement checkpoint history",
                ));
            }
            if pending {
                self.pending = Some(capture);
            } else {
                self.latest = Some((capture, stamp));
            }
        }
        if let Some((ordinal, file)) = tip {
            let (capture, stamp) = self.latest.as_ref().expect("epoch base exists");
            if ordinal != capture.ordinal {
                return Err(bad("checkpoint tip differs from its parsed history"));
            }
            let name = format!("{ordinal:016}.json");
            cover_frozen(directory, &name, &file, &capture.encode()?, stamp)?;
            self.tip_file = Some(file);
        }
        // Every predecessor must still have the physical stamp named by its successor.
        // This is an equality recheck, never fresh authority for changed historical bytes.
        for (name, held, stamp) in checked {
            if Stamp::read(&held)? != stamp
                || Stamp::read(&open(directory, &name, false, false)?)? != stamp
            {
                return Err(bad("checkpoint history changed after its semantic read"));
            }
        }
        if directory_origin(directory)? != before {
            return Err(bad("checkpoint directory changed during its complete read"));
        }
        epoch.recheck(&self.anchor.directory)
    }

    pub(super) fn missing_retired(&self) -> bool {
        self.pending.is_none() && self.latest.as_ref().is_some_and(|(c, _)| c.retired)
    }

    pub(super) fn working_name(&self) -> Option<&str> {
        self.latest
            .as_ref()
            .map(|(capture, _)| capture.name.as_str())
    }

    pub(super) fn scan_name(&self) -> io::Result<&str> {
        if self.pending.is_some() {
            return Err(bad("active claim has a pending generation"));
        }
        self.working_name()
            .ok_or_else(|| bad("orphan active claim has no committed epoch"))
    }

    pub(super) fn matches_claim(
        &self,
        marker: &str,
        guard: ClaimGuardIdentity,
        namespace: &str,
    ) -> io::Result<bool> {
        let mut found = false;
        for capture in self
            .latest
            .as_ref()
            .map(|(c, _)| c)
            .into_iter()
            .chain(self.pending.as_ref())
        {
            let (record, _) =
                Journal::decode(&capture.text, capture.name.ends_with(CLAIM), namespace)?;
            if record.cover_marker() != marker
                || capture.origin.0[0].as_u64() != Some(guard.device)
                || capture.origin.0[1].as_u64() != Some(guard.inode)
            {
                return Ok(false);
            }
            found = true;
        }
        Ok(found)
    }

    pub(super) fn claim_state(
        &self,
        backend: &File,
        marker: &str,
        guard: ClaimGuardIdentity,
        namespace: &str,
    ) -> io::Result<CohortClaimState> {
        if !self.matches_claim(marker, guard, namespace)? || self.pending.is_some() {
            return Ok(CohortClaimState::Unknown);
        }
        let (latest, _) = self
            .latest
            .as_ref()
            .ok_or_else(|| bad("claim has no committed generation"))?;
        match open(backend, &latest.name, false, false) {
            Ok(file) => {
                latest.check(backend, &file)?;
                Ok(if latest.retired {
                    CohortClaimState::Closed
                } else {
                    CohortClaimState::Retained
                })
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(if latest.retired {
                CohortClaimState::Closed
            } else {
                CohortClaimState::Unknown
            }),
            Err(error) => Err(error),
        }
    }

    pub(super) fn active_closed(
        &self,
        namespace: &crate::cohort_namespace::Namespace,
        reference: &crate::CohortClosureRef,
    ) -> io::Result<bool> {
        archive::active_closed(self, namespace, reference)
    }

    pub(super) fn archive(
        &self,
        namespace: &crate::cohort_namespace::Namespace,
        guard: &RecordLock,
    ) -> io::Result<()> {
        archive::publish(self, namespace, guard)
    }

    /// Complete only the already captured intent, retaining the parsed metadata FD and the
    /// recorded working origin. This grants no writer, birth or no-child capability.
    pub(super) fn recover_pending(&mut self, backend: &File, namespace: &str) -> io::Result<()> {
        let Some(pending) = self.pending.clone() else {
            return Ok(());
        };
        let (_, journal) =
            Journal::decode(&pending.text, pending.name.ends_with(CLAIM), namespace)?;
        if let Some((latest, _)) = &self.latest {
            Journal::decode(&latest.text, latest.name.ends_with(CLAIM), namespace)?;
            if !known_record_update(&latest.text, &pending.text)
                || pending.origin.0[..2] != latest.origin.0[..2]
                || pending.origin.0[3..6] != latest.origin.0[3..6]
                || (latest.name.ends_with(CLAIM) && pending.name != latest.name)
            {
                return Err(bad(
                    "pending checkpoint is not an exact forward update of its committed claim",
                ));
            }
        } else if pending.retired {
            return Err(bad("retirement has no committed predecessor"));
        }
        let record = open(backend, &pending.name, false, false)?;
        pending.check(backend, &record)?;
        if pending.retired {
            if journal.lifetime() != crate::cohort::Lifetime::Drained {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "pending retirement has no actual scope drain",
                ));
            }
            // This record was freshly opened above, never duped from an inherited guard.
            // Keep its exclusive description through publication so an alias cannot arm.
            let lock = RecordLock::try_lock(record, RECORD_MAX).map_err(|error| match error {
                std::fs::TryLockError::WouldBlock => io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "pending retirement still has a held guard",
                ),
                std::fs::TryLockError::Error(error) => error,
            })?;
            pending.check(backend, &lock)?;
            if journal.lifetime() != crate::cohort::Lifetime::Drained {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "pending retirement scope is no longer conclusively drained",
                ));
            }
            return self.commit(backend, &lock, &pending);
        }
        self.commit(backend, &record, &pending)
    }

    /// Rotate BEFORE changing the working record. Each epoch keeps one slot exclusively
    /// for terminal publication; an incomplete prune/publication blocks the next mutation.
    pub(super) fn ensure_room(&mut self, backend: &File, writer: &File) -> io::Result<()> {
        if self.pending.is_some() {
            return Err(bad("pending generation cannot rotate"));
        }
        let epoch = self
            .epoch
            .as_ref()
            .ok_or_else(|| bad("working claim has no epoch"))?;
        epoch.recheck(&self.anchor.directory)?;
        let (capture, stamp) = self
            .latest
            .as_ref()
            .ok_or_else(|| bad("working claim has no tip"))?;
        if capture.ordinal.checked_add(1).is_none() {
            return Err(bad("generation ordinal exhausted before working mutation"));
        }
        capture.check(backend, writer)?;
        if capture.retired {
            return Err(bad("retired claim cannot rotate"));
        }
        if capture
            .ordinal
            .checked_sub(epoch.base.ordinal)
            .ok_or_else(|| bad("tip precedes its epoch"))?
            + 1
            < (epoch::WINDOW - 1) as u64
        {
            return Ok(());
        }
        let next = epoch.rotate(
            &self.anchor.directory,
            backend,
            writer,
            capture,
            self.tip_file
                .as_ref()
                .ok_or_else(|| bad("tip lost its original descriptor"))?,
            stamp,
        )?;
        let (file, stamp) = next.base_descriptor()?;
        self.latest = Some((next.base.clone(), stamp));
        self.tip_file = Some(file);
        self.epoch = Some(next);
        Ok(())
    }

    pub(super) fn observe(&self, backend: &File, writer: &File, name: &str) -> io::Result<String> {
        if self.pending.is_some() {
            return Err(bad("cohort generation publication is pending"));
        }
        let (latest, _) = self
            .latest
            .as_ref()
            .ok_or_else(|| bad("cohort has no committed generation"))?;
        if latest.retired || latest.name != name {
            return Err(bad(
                "cohort generation is retired or renamed outside custody",
            ));
        }
        latest.check(backend, writer)?;
        Ok(latest.text.clone())
    }

    pub(super) fn retired(&self, backend: &File, writer: &File, name: &str) -> io::Result<bool> {
        if self.pending.is_some() {
            return Err(bad("cohort generation publication is pending"));
        }
        let (latest, _) = self
            .latest
            .as_ref()
            .ok_or_else(|| bad("cohort has no committed generation"))?;
        if latest.name != name {
            return Err(bad("cohort generation name changed"));
        }
        latest.check(backend, writer)?;
        Ok(latest.retired)
    }

    /// Freeze the intended bytes and origin BEFORE the covering flush. The caller holds the
    /// transaction throughout the known write/rename, capture, flush and publication.
    pub(super) fn capture(
        &self,
        backend: &File,
        writer: &File,
        name: &str,
        expected: &str,
        retired: bool,
    ) -> io::Result<Capture> {
        if self.pending.is_some() || self.latest.as_ref().is_some_and(|(c, _)| c.retired) {
            return Err(bad("cannot replace pending or retired generation"));
        }
        let ordinal = match self.latest.as_ref() {
            Some((capture, _)) => capture
                .ordinal
                .checked_add(1)
                .ok_or_else(|| bad("generation ordinal exhausted"))?,
            None => 1,
        };
        if self
            .epoch
            .as_ref()
            .is_some_and(|epoch| ordinal.saturating_sub(epoch.base.ordinal) >= epoch::WINDOW as u64)
        {
            return Err(bad("cohort checkpoint epoch is full"));
        }
        if stem(name)? != self.anchor.stem {
            return Err(bad("cohort generation names another anchored claim"));
        }
        let capture = Capture {
            ordinal,
            name: name.to_owned(),
            text: expected.to_owned(),
            origin: Stamp::read(writer)?,
            previous: self.latest.as_ref().map(|(_, s)| s.clone()),
            retired,
        };
        if let Some((latest, _)) = &self.latest
            && (capture.origin.0[..2] != latest.origin.0[..2]
                || capture.origin.0[3..6] != latest.origin.0[3..6])
        {
            return Err(bad("cohort generation replaced its original working inode"));
        }
        capture.check(backend, writer)?;
        Ok(capture)
    }

    pub(super) fn commit(
        &mut self,
        backend: &File,
        writer: &File,
        capture: &Capture,
    ) -> io::Result<()> {
        self.anchor.validate(backend)?;
        capture.check(backend, writer)?;
        if self.epoch.is_none() {
            if self.latest.is_some() || self.pending.is_some() {
                return Err(bad("initial epoch has stale generation state"));
            }
            aterm_durable::durable(writer)?;
            aterm_durable::durable(backend)?;
            #[cfg(test)]
            after_flush(backend, writer)?;
            capture.check(backend, writer)?;
            let epoch = Epoch::first(&self.anchor.directory, backend, writer, capture)?;
            let (file, stamp) = epoch.base_descriptor()?;
            self.latest = Some((capture.clone(), stamp));
            self.tip_file = Some(file);
            self.epoch = Some(epoch);
            return Ok(());
        }
        let epoch = self.epoch.as_ref().expect("existing epoch");
        epoch.recheck(&self.anchor.directory)?;
        let metadata = &epoch.directory;
        let final_name = format!("{:016}.json", capture.ordinal);
        let pending_name = format!("{:016}.pending", capture.ordinal);
        let encoded = capture.encode()?;
        if let Some(pending) = &self.pending {
            if pending != capture {
                return Err(bad("another cohort generation is pending"));
            }
        } else if let Some((latest, _)) = &self.latest
            && latest.ordinal == capture.ordinal
        {
            if latest != capture {
                return Err(bad("cohort generation retry differs"));
            }
            if capture.ordinal == epoch.base.ordinal {
                epoch.cover_base(&self.anchor.directory)?;
                self.anchor.validate(backend)?;
                return capture.check(backend, writer);
            }
            // A prior interruption may leave a final name without an acknowledged flush.
            // Cover the exact metadata FILE as well as its directory, even on this retry.
            cover_frozen(
                metadata,
                &final_name,
                self.tip_file
                    .as_ref()
                    .ok_or_else(|| bad("final retry lost its parsed metadata descriptor"))?,
                &encoded,
                &self.latest.as_ref().expect("final generation").1,
            )?;
            self.anchor.validate(backend)?;
            return capture.check(backend, writer);
        } else {
            if capture.previous.as_ref() != self.latest.as_ref().map(|(_, s)| s)
                || capture.ordinal != self.latest.as_ref().map_or(1, |(c, _)| c.ordinal + 1)
            {
                return Err(bad("stale cohort generation writer"));
            }
            let mut file = open(metadata, &pending_name, true, false)?;
            file.write_all(encoded.as_bytes())?;
            fd::fchmod(&file, Mode::RUSR)?;
            let stamp = Stamp::read(&file)?;
            self.pending_file = Some((file, stamp));
            self.pending = Some(capture.clone());
        }
        let (held, publication_origin) = self
            .pending_file
            .as_ref()
            .ok_or_else(|| bad("pending retry lost its parsed metadata descriptor"))?;
        cover_frozen(metadata, &pending_name, held, &encoded, publication_origin)?;
        let publication = held.try_clone()?;
        let publication_origin = publication_origin.clone();
        aterm_durable::durable(writer)?;
        aterm_durable::durable(backend)?;
        #[cfg(test)]
        after_flush(backend, writer)?;
        capture.check(backend, writer)?;
        #[cfg(test)]
        if FAIL_BEFORE_PUBLISH.with(|f| f.replace(false)) {
            return Err(io::Error::other(
                "injected checkpoint publication interruption",
            ));
        }
        let pending = PendingPublication {
            metadata,
            pending_name: &pending_name,
            final_name: &final_name,
            file: &publication,
            origin: &publication_origin,
            encoded: &encoded,
        };
        let stamp = if capture.retired {
            publish_checkpoint(pending, backend, writer, capture)?
        } else {
            append_checkpoint(pending, backend, writer, capture)?
        };
        self.latest = Some((capture.clone(), stamp));
        self.tip_file = Some(publication);
        self.pending = None;
        self.pending_file = None;
        self.anchor.validate(backend)?;
        epoch.recheck(&self.anchor.directory)?;
        Ok(())
    }
}

#[cfg(test)]
type PublicationAfterCover = fn(&File, &str, &File, &Stamp) -> io::Result<()>;
#[cfg(test)]
thread_local! {
    static AFTER_CHECKPOINT_COVER: std::cell::Cell<Option<PublicationAfterCover>> = const {
        std::cell::Cell::new(None)
    };
}

struct PendingPublication<'a> {
    metadata: &'a File,
    pending_name: &'a str,
    final_name: &'a str,
    file: &'a File,
    origin: &'a Stamp,
    encoded: &'a str,
}

/// Ordinary generations add one physical checkpoint. A retired generation instead uses
/// the reserved terminal slot through the same mechanics without claiming an Append step.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CapacityHistory",
        action = "Append",
        project = "aterm_capacity::ledger::cohort_ledger::checkpoint::history_model_tests::project"
    )
)]
fn append_checkpoint(
    pending: PendingPublication<'_>,
    backend: &File,
    writer: &File,
    capture: &Capture,
) -> io::Result<Stamp> {
    publish_checkpoint(pending, backend, writer, capture)
}

fn publish_checkpoint(
    pending: PendingPublication<'_>,
    backend: &File,
    writer: &File,
    capture: &Capture,
) -> io::Result<Stamp> {
    let PendingPublication {
        metadata,
        pending_name,
        final_name,
        file: publication,
        origin: publication_origin,
        encoded,
    } = pending;
    if Stamp::read(publication)? != *publication_origin
        || Stamp::read(&open(metadata, pending_name, false, false)?)? != *publication_origin
    {
        return Err(bad("checkpoint metadata changed before its known rename"));
    }
    fd::renameat_with(
        metadata,
        pending_name,
        metadata,
        final_name,
        RenameFlags::NOREPLACE,
    )?;
    // Rename may change only ctime. Freeze that owned transition BEFORE any covering
    // IO or observer; a later same-byte restore is not part of the rename.
    let renamed = Stamp::read(publication)?;
    if renamed.0[..8] != publication_origin.0[..8]
        || Stamp::read(&open(metadata, final_name, false, false)?)? != renamed
    {
        return Err(bad(
            "checkpoint rename changed more than its original ctime",
        ));
    }
    aterm_durable::durable(metadata)?;
    #[cfg(test)]
    if let Some(observe) = AFTER_CHECKPOINT_COVER.with(std::cell::Cell::take) {
        observe(metadata, final_name, publication, &renamed)?;
    }
    capture.check(backend, writer)?;
    let (published, stamp) = bytes(publication, MAX_ENTRY)?;
    if published != encoded
        || stamp != renamed
        || Stamp::read(&open(metadata, final_name, false, false)?)? != renamed
    {
        return Err(bad("checkpoint known rename changed its original payload"));
    }
    Ok(renamed)
}

pub(super) fn archived_state(
    namespace: &crate::cohort_namespace::Namespace,
    reference: &crate::CohortClosureRef,
    reap: bool,
) -> io::Result<CohortClaimState> {
    archive::query(namespace, reference, reap)
}

/// Recovery accepts append/measured-write updates or a complete custody snapshot with
/// exactly equal semantics and unchanged ordinary accounting. A snapshot cannot discard
/// an unknown ARM or release a scope; retirement is a preceding committed event.
fn known_record_update(before: &str, after: &str) -> bool {
    if Journal::known_subgroup_compaction(before, after) {
        return true;
    }
    if !before.ends_with('\n') || !after.ends_with('\n') {
        return false;
    }
    let old = before.split_inclusive('\n').collect::<Vec<_>>();
    let new = after.split_inclusive('\n').collect::<Vec<_>>();
    if old.is_empty() || new.len() < old.len() || old[0] != new[0] {
        return false;
    }
    let mut replaced = 0;
    for (left, right) in old.iter().zip(&new) {
        if left == right {
            continue;
        }
        let wrote = |line: &str| {
            line.len() == super::super::WROTE_LINE
                && aterm_json::from_str::<Value>(line).ok().is_some_and(|v| {
                    v.as_object().is_some_and(|m| m.len() == 1)
                        && v.get("wrote").and_then(Value::as_u64).is_some()
                })
        };
        if !wrote(left) || !wrote(right) {
            return false;
        }
        replaced += 1;
    }
    replaced <= 1
}

fn directory_origin(file: &File) -> io::Result<(u64, u64, u32, i64, i64, i64, i64)> {
    let m = file.metadata()?;
    Ok((
        m.dev(),
        m.ino(),
        m.mode(),
        m.ctime(),
        m.ctime_nsec(),
        m.mtime(),
        m.mtime_nsec(),
    ))
}

/// Validate every checkpoint directory, including metadata whose working record disappeared.
/// A retired tombstone may outlive its payload. Every other orphan, gap or pending step blocks.
#[cfg(test)]
pub(super) fn validate_metadata(backend: &File, name: &str, nonce: &str) -> io::Result<()> {
    validate_metadata_with_recovery(backend, name, nonce, false)
}

#[cfg(test)]
pub(super) fn validate_metadata_with_recovery(
    backend: &File,
    name: &str,
    nonce: &str,
    recover: bool,
) -> io::Result<()> {
    let anchor = Anchor::open_name(backend, name, nonce)?;
    let directory = anchor.directory.try_clone()?;
    let backend = &directory;
    let mut transaction = anchor.lock(backend)?;
    if recover {
        transaction.recover_pending(backend, nonce)?;
    }
    if transaction.pending.is_some() {
        return Err(bad("orphan or pending cohort generation"));
    }
    let (latest, _) = transaction
        .latest
        .as_ref()
        .ok_or_else(|| bad("orphan cohort checkpoint anchor"))?;
    match open(backend, &latest.name, false, false) {
        Ok(file) => latest.check(backend, &file),
        Err(e) if e.kind() == io::ErrorKind::NotFound && latest.retired => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
thread_local! {
    static AFTER_FLUSH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static FAIL_BEFORE_PUBLISH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static FAIL_PUBLICATION_FLUSH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static FAIL_PENDING_FILE_FLUSH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static FAIL_NAMED_FILE_FLUSH:std::cell::Cell<Option<&'static str>>=const {std::cell::Cell::new(None)};
    static FAIL_EXACT_FILE_FLUSH:std::cell::Cell<Option<(u64,u64)>>=const {std::cell::Cell::new(None)};
}

#[cfg(test)]
fn after_flush(backend: &File, writer: &File) -> io::Result<()> {
    if AFTER_FLUSH.with(|f| f.replace(false)) {
        past_the_clock_tick(backend, writer);
        let (text, _) = bytes(writer, RECORD_MAX as usize)?;
        writer.write_all_at(b"!", 0)?;
        writer.write_all_at(text.as_bytes(), 0)?;
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn restore_after_next_flush() {
    AFTER_FLUSH.with(|f| f.set(true));
}

#[cfg(test)]
pub(super) fn interrupt_next_publication() {
    FAIL_BEFORE_PUBLISH.with(|f| f.set(true));
}

#[cfg(test)]
pub(super) fn refuse_next_pending_file_flush() {
    FAIL_PENDING_FILE_FLUSH.with(|f| f.set(true));
}

#[cfg(test)]
pub(super) fn interrupt_archive(stage: &'static str) {
    archive::interrupt(stage)
}

#[cfg(test)]
pub(super) fn interrupt_epoch(stage: &'static str) {
    epoch::interrupt(stage)
}

#[cfg(test)]
pub(super) fn refuse_named_file_flush(name: &'static str) {
    FAIL_NAMED_FILE_FLUSH.with(|slot| slot.set(Some(name)));
}

#[cfg(test)]
pub(super) fn with_archive_gate<T>(
    namespace: &crate::cohort_namespace::Namespace,
    reference: &crate::CohortClosureRef,
    prepare: bool,
    action: impl FnOnce() -> T,
) -> io::Result<T> {
    archive::with_gate(namespace, reference, prepare, action)
}

#[cfg(test)]
pub(super) fn replace_archive_after_cover(
    namespace: &crate::cohort_namespace::Namespace,
    reference: &crate::CohortClosureRef,
    phase: &'static str,
) -> io::Result<std::path::PathBuf> {
    archive::replace_after_cover(namespace, reference, phase)
}

#[cfg(test)]
pub(super) fn transient_archive_after_cover(
    namespace: &crate::cohort_namespace::Namespace,
    reference: &crate::CohortClosureRef,
    phase: &'static str,
) -> io::Result<()> {
    archive::transient_after_cover(namespace, reference, phase)
}

#[cfg(test)]
pub(super) fn interrupt_slot_preparation(stage: &'static str) {
    archive::interrupt_preparation(stage)
}

/// Establish the filesystem clock witness BEFORE touching the victim. The one-minute bound
/// detects a clock that never advances; it is not a latency assertion or a write retry.
#[cfg(test)]
pub(super) fn past_the_clock_tick(backend: &File, victim: &File) {
    let before = Stamp::read(victim).unwrap();
    let m = victim.metadata().unwrap();
    let victim_tick = (m.ctime(), m.ctime_nsec());
    let observed = aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the cohort fixture filesystem clock to pass the untouched victim",
        || {
            let name = format!(".tick-{}", aterm_uds::rand::hex_token::<8>().unwrap());
            fd::mkdirat(backend, name.as_str(), Mode::RWXU).unwrap();
            let probe = open(backend, &name, false, true).unwrap();
            let m = probe.metadata().unwrap();
            let tick = (m.ctime(), m.ctime_nsec());
            drop(probe);
            fd::unlinkat(backend, name.as_str(), fd::AtFlags::REMOVEDIR).unwrap();
            (tick > victim_tick).then_some(tick)
        },
    );
    assert!(observed > victim_tick);
    assert_eq!(
        Stamp::read(victim).unwrap(),
        before,
        "clock probing must leave the victim untouched"
    );
}

/// Keep an actual failed filesystem world, including its random generation identities.
/// Successful controls remove only their own temporary data as before.
#[cfg(test)]
pub(super) struct TestWorld(Option<aterm_tempfile::TempDir>);

#[cfg(test)]
impl TestWorld {
    pub(super) fn new(label: &str) -> Self {
        // Each ledger operation reopens and validates the namespace's directories, so a world's
        // test is file system heavy: it takes a place in the lane.
        aterm_spec::fs_lane::join();
        let temporary = aterm_tempfile::tempdir().unwrap();
        eprintln!(
            "capacity {label} fixture world: {}",
            temporary.path().display()
        );
        Self(Some(temporary))
    }

    pub(super) fn path(&self) -> &std::path::Path {
        self.0.as_ref().expect("retained fixture").path()
    }
}

#[cfg(test)]
impl Drop for TestWorld {
    fn drop(&mut self) {
        if std::thread::panicking()
            && let Some(temporary) = self.0.take()
        {
            eprintln!(
                "capacity failed fixture retained: {}",
                temporary.keep().display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};

    struct Fixture {
        scratch: TestWorld,
        backend: File,
        writer: File,
        anchor: Anchor,
        name: String,
    }

    impl Fixture {
        fn new() -> Self {
            let scratch = TestWorld::new("checkpoint");
            let root = scratch.path().join("ledger.noindex");
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&root)
                .unwrap();
            let parent = File::open(&root).unwrap();
            let name = "0000000000001-1-1.claim".to_owned();
            let (anchor, writer) = Anchor::create(&parent, &name, "namespace").unwrap();
            let backend = anchor.directory.try_clone().unwrap();
            writer.write_all_at(b"head\n", 0).unwrap();
            let mut transaction = anchor.lock(&backend).unwrap();
            let capture = transaction
                .capture(&backend, &writer, &name, "head\n", false)
                .unwrap();
            transaction.commit(&backend, &writer, &capture).unwrap();
            drop(transaction);
            Self {
                scratch,
                backend,
                writer,
                anchor,
                name,
            }
        }

        fn root(&self) -> std::path::PathBuf {
            self.scratch.path().join("ledger.noindex")
        }

        fn record_path(&self) -> std::path::PathBuf {
            self.root().join(&self.anchor.name).join(&self.name)
        }

        fn metadata_path(&self, name: &str) -> std::path::PathBuf {
            let root = self.root().join(&self.anchor.name);
            if matches!(name, HEADER | MUTATION) {
                return root.join(name);
            }
            root.join("epoch-0000000000000001.noindex")
                .join(if name == "0000000000000001.json" {
                    "base.json"
                } else {
                    name
                })
        }

        fn validate(&self) -> io::Result<()> {
            validate_metadata(&File::open(self.root())?, &self.anchor.name, "namespace")
        }

        fn change(&self, transaction: &mut Transaction, text: &str) -> Capture {
            self.writer.write_all_at(text.as_bytes(), 0).unwrap();
            self.writer.set_len(text.len() as u64).unwrap();
            transaction
                .capture(&self.backend, &self.writer, &self.name, text, false)
                .unwrap()
        }
    }

    fn replace_metadata(f: &Fixture, name: &str, copied: bool) {
        let path = f.metadata_path(name);
        let original = std::fs::read(&path).unwrap();
        let before = Stamp::read(&File::open(&path).unwrap()).unwrap();
        if copied {
            let copy = f.root().join("copied-checkpoint");
            std::fs::copy(&path, &copy).unwrap();
            std::fs::rename(copy, &path).unwrap();
            assert_ne!(
                Stamp::read(&File::open(&path).unwrap()).unwrap().0[..2],
                before.0[..2]
            );
        } else {
            past_the_clock_tick(&f.backend, &File::open(&path).unwrap());
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            let writer = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            writer.write_all_at(b"!", 0).unwrap();
            writer.write_all_at(&original, 0).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();
            let after = Stamp::read(&writer).unwrap();
            assert_eq!(after.0[..2], before.0[..2]);
            assert_ne!(after, before);
        }
        assert_eq!(
            std::fs::read(&path).unwrap(),
            original,
            "CONTROL: canonical bytes alone would accept the metadata"
        );
    }

    #[test]
    fn checkpoint_cover_cannot_recapture_a_same_byte_restore_as_its_known_rename() {
        thread_local! { static RESTORE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
        fn after_cover(parent: &File, name: &str, file: &File, renamed: &Stamp) -> io::Result<()> {
            assert_eq!(Stamp::read(file)?, *renamed);
            assert_eq!(Stamp::read(&open(parent, name, false, false)?)?, *renamed);
            if RESTORE.with(std::cell::Cell::take) {
                let before = file.metadata()?;
                let text = bytes(file, MAX_ENTRY)?.0;
                past_the_clock_tick(parent, file);
                file.write_all_at(text.as_bytes(), 0)?;
                file.set_modified(before.modified()?)?;
                let restored = Stamp::read(file)?;
                assert_eq!(restored.0[..8], renamed.0[..8]);
                assert_ne!(restored, *renamed);
                assert_eq!(bytes(file, MAX_ENTRY)?.0, text);
            }
            Ok(())
        }
        for retired in [false, true] {
            for restore in [false, true] {
                let f = Fixture::new();
                let mut transaction = f.anchor.lock(&f.backend).unwrap();
                let capture = transaction
                    .capture(&f.backend, &f.writer, &f.name, "head\n", retired)
                    .unwrap();
                RESTORE.with(|flag| flag.set(restore));
                AFTER_CHECKPOINT_COVER.with(|hook| hook.set(Some(after_cover)));
                let committed = transaction.commit(&f.backend, &f.writer, &capture);
                assert!(AFTER_CHECKPOINT_COVER.with(std::cell::Cell::take).is_none());
                if restore {
                    assert_eq!(
                        committed.unwrap_err().kind(),
                        io::ErrorKind::InvalidData,
                        "a restore during parent cover cannot become the rename's frozen stamp"
                    );
                } else {
                    committed.unwrap();
                    drop(transaction);
                    // A normal known rename remains usable through a fresh physical read.
                    let fresh = f.anchor.lock(&f.backend).unwrap();
                    let (observed, stamp) = fresh.latest.as_ref().unwrap();
                    let tip = fresh.tip_file.as_ref().unwrap();
                    assert_eq!(observed, &capture);
                    assert_eq!(Stamp::read(tip).unwrap(), *stamp);
                    assert_eq!(bytes(tip, MAX_ENTRY).unwrap().0, capture.encode().unwrap());
                }
            }
        }
    }

    #[test]
    fn cold_tip_authority_covers_its_exact_file_including_retired_tombstones() {
        for copied in [false, true] {
            for retired in [false, true] {
                let f = Fixture::new();
                let ordinal = if retired {
                    let mut transaction = f.anchor.lock(&f.backend).unwrap();
                    let capture = transaction
                        .capture(&f.backend, &f.writer, &f.name, "head\n", true)
                        .unwrap();
                    transaction.commit(&f.backend, &f.writer, &capture).unwrap();
                    drop(transaction);
                    std::fs::remove_file(f.record_path()).unwrap();
                    2
                } else {
                    1
                };
                replace_metadata(&f, &format!("{ordinal:016}.json"), copied);
                let cold = Anchor::open(&f.backend, &f.name, "namespace").unwrap();
                FAIL_NAMED_FILE_FLUSH.with(|hook| {
                    hook.set(Some(if retired {
                        "0000000000000002.json"
                    } else {
                        "base.json"
                    }))
                });
                assert!(
                    cold.lock(&f.backend).is_err(),
                    "cold tip/tombstone cannot authorize anything after its FILE flush was refused"
                );
                let recovered = cold.lock(&f.backend).unwrap();
                if retired {
                    assert!(recovered.missing_retired());
                    drop(recovered);
                    f.validate().unwrap();
                } else {
                    assert_eq!(
                        recovered.observe(&f.backend, &f.writer, &f.name).unwrap(),
                        "head\n"
                    );
                }
            }
        }
    }

    #[test]
    fn cold_header_cover_refuses_failure_then_recovers_without_rebasing_the_record() {
        for copied in [false, true] {
            let f = Fixture::new();
            replace_metadata(&f, HEADER, copied);
            assert!(
                f.anchor.lock(&f.backend).is_err(),
                "the original live header proof was revoked"
            );
            FAIL_PUBLICATION_FLUSH.with(|hook| hook.set(true));
            assert!(Anchor::open(&f.backend, &f.name, "namespace").is_err());
            let recovered = Anchor::open(&f.backend, &f.name, "namespace").unwrap();
            assert_eq!(
                recovered
                    .lock(&f.backend)
                    .unwrap()
                    .observe(&f.backend, &f.writer, &f.name)
                    .unwrap(),
                "head\n"
            );

            past_the_clock_tick(&f.backend, &f.writer);
            f.writer.write_all_at(b"!", 0).unwrap();
            f.writer.write_all_at(b"head\n", 0).unwrap();
            let cold = Anchor::open(&f.backend, &f.name, "namespace").unwrap();
            assert!(
                cold.lock(&f.backend)
                    .unwrap()
                    .observe(&f.backend, &f.writer, &f.name)
                    .is_err(),
                "fresh header/tip cover cannot refresh the working record's old origin"
            );
        }
    }

    #[test]
    fn cold_cover_cannot_refresh_a_predecessor_stamp_named_by_its_successor() {
        for copied in [false, true] {
            let f = Fixture::new();
            let mut transaction = f.anchor.lock(&f.backend).unwrap();
            let capture = f.change(&mut transaction, "next\n");
            transaction.commit(&f.backend, &f.writer, &capture).unwrap();
            drop(transaction);
            replace_metadata(&f, "0000000000000001.json", copied);
            let cold = Anchor::open(&f.backend, &f.name, "namespace").unwrap();
            assert!(
                cold.lock(&f.backend).is_err(),
                "covering the tip never rebases a changed physical predecessor"
            );
        }
    }

    #[test]
    fn equal_bytes_on_the_same_inode_do_not_restore_a_committed_origin() {
        let f = Fixture::new();
        let before = Stamp::read(&f.writer).unwrap();
        past_the_clock_tick(&f.backend, &f.writer);
        f.writer.write_all_at(b"!", 0).unwrap();
        f.writer.write_all_at(b"head\n", 0).unwrap();
        let (text, after) = bytes(&f.writer, MAX_ENTRY).unwrap();
        assert_eq!(
            text, "head\n",
            "CONTROL: byte equality would accept this restore"
        );
        assert_eq!(
            before.0[..2],
            after.0[..2],
            "the original inode was retained"
        );
        assert_ne!(
            before, after,
            "the real write/restore changed physical origin"
        );
        assert!(
            f.anchor
                .lock(&f.backend)
                .unwrap()
                .observe(&f.backend, &f.writer, &f.name)
                .is_err()
        );
        let cold = Anchor::open(&f.backend, &f.name, "namespace").unwrap();
        assert!(
            cold.lock(&f.backend)
                .unwrap()
                .observe(&f.backend, &f.writer, &f.name)
                .is_err()
        );
    }

    #[test]
    fn copied_payload_and_copied_checkpoint_directory_cannot_replace_the_originals() {
        let f = Fixture::new();
        let old = f.root().join("old-record");
        std::fs::rename(f.record_path(), &old).unwrap();
        std::fs::copy(&old, f.record_path()).unwrap();
        let replacement = open(&f.backend, &f.name, false, false).unwrap();
        assert_eq!(
            bytes(&replacement, MAX_ENTRY).unwrap().0,
            bytes(&f.writer, MAX_ENTRY).unwrap().0,
            "CONTROL: the copy has equal record bytes"
        );
        assert_ne!(
            Stamp::read(&replacement).unwrap().0[..2],
            Stamp::read(&f.writer).unwrap().0[..2]
        );
        assert!(f.anchor.lock(&f.backend).is_err());
        assert!(Anchor::open(&f.backend, &f.name, "namespace").is_err());

        let g = Fixture::new();
        let old_metadata = g.root().join("old-metadata");
        std::fs::rename(g.root().join(&g.anchor.name), &old_metadata).unwrap();
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(g.root().join(&g.anchor.name))
            .unwrap();
        for name in [HEADER, MUTATION, g.name.as_str()] {
            std::fs::copy(
                old_metadata.join(name),
                g.root().join(&g.anchor.name).join(name),
            )
            .unwrap();
        }
        let epoch = "epoch-0000000000000001.noindex";
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(g.root().join(&g.anchor.name).join(epoch))
            .unwrap();
        std::fs::copy(
            old_metadata.join(epoch).join("base.json"),
            g.root().join(&g.anchor.name).join(epoch).join("base.json"),
        )
        .unwrap();
        let copied = File::open(g.root().join(&g.anchor.name)).unwrap();
        assert!(Anchor::open(&copied, &g.name, "namespace").is_err());
        assert!(g.anchor.lock(&g.backend).is_err());
    }

    #[test]
    fn restore_during_the_covering_flush_cannot_be_captured_late() {
        let f = Fixture::new();
        let mut transaction = f.anchor.lock(&f.backend).unwrap();
        let capture = f.change(&mut transaction, "next\n");
        AFTER_FLUSH.with(|hook| hook.set(true));
        assert!(transaction.commit(&f.backend, &f.writer, &capture).is_err());
        assert_eq!(bytes(&f.writer, MAX_ENTRY).unwrap().0, "next\n");
        assert!(capture.check(&f.backend, &f.writer).is_err());
        drop(transaction);
        let mut recovered = f.anchor.lock(&f.backend).unwrap();
        assert!(recovered.observe(&f.backend, &f.writer, &f.name).is_err());
        assert!(recovered.commit(&f.backend, &f.writer, &capture).is_err());
    }

    #[test]
    fn interrupted_publication_retries_only_its_original_frozen_generation() {
        let f = Fixture::new();
        let mut transaction = f.anchor.lock(&f.backend).unwrap();
        let capture = f.change(&mut transaction, "next\n");
        FAIL_BEFORE_PUBLISH.with(|hook| hook.set(true));
        assert!(transaction.commit(&f.backend, &f.writer, &capture).is_err());
        drop(transaction);
        let mut recovered = f.anchor.lock(&f.backend).unwrap();
        assert!(recovered.observe(&f.backend, &f.writer, &f.name).is_err());
        assert!(
            recovered
                .capture(&f.backend, &f.writer, &f.name, "next\n", false)
                .is_err(),
            "a retry cannot mint a later origin"
        );
        recovered.commit(&f.backend, &f.writer, &capture).unwrap();
        assert_eq!(
            recovered.observe(&f.backend, &f.writer, &f.name).unwrap(),
            "next\n"
        );
    }

    #[test]
    fn an_existing_pending_file_is_not_proof_of_its_own_covering_flush() {
        let f = Fixture::new();
        let mut transaction = f.anchor.lock(&f.backend).unwrap();
        let capture = f.change(&mut transaction, "next\n");
        FAIL_PUBLICATION_FLUSH.with(|hook| hook.set(true));
        assert!(transaction.commit(&f.backend, &f.writer, &capture).is_err());
        drop(transaction);
        let mut recovered = f.anchor.lock(&f.backend).unwrap();
        assert!(recovered.observe(&f.backend, &f.writer, &f.name).is_err());
        FAIL_PUBLICATION_FLUSH.with(|hook| hook.set(true));
        assert!(
            recovered.commit(&f.backend, &f.writer, &capture).is_err(),
            "retry must cover the exact metadata FILE too"
        );
        recovered.commit(&f.backend, &f.writer, &capture).unwrap();
        assert_eq!(
            recovered.observe(&f.backend, &f.writer, &f.name).unwrap(),
            "next\n"
        );
        FAIL_PUBLICATION_FLUSH.with(|hook| hook.set(true));
        assert!(
            recovered.commit(&f.backend, &f.writer, &capture).is_err(),
            "an existing final name also requires its file flush"
        );
        recovered.commit(&f.backend, &f.writer, &capture).unwrap();
    }

    #[test]
    fn torn_pending_history_and_a_missing_predecessor_block_cold_readers() {
        let f = Fixture::new();
        let mut transaction = f.anchor.lock(&f.backend).unwrap();
        let capture = f.change(&mut transaction, "next\n");
        FAIL_BEFORE_PUBLISH.with(|hook| hook.set(true));
        assert!(transaction.commit(&f.backend, &f.writer, &capture).is_err());
        drop(transaction);
        let pending = f.metadata_path("0000000000000002.pending");
        let mut permissions = std::fs::metadata(&pending).unwrap().permissions();
        use std::os::unix::fs::PermissionsExt as _;
        permissions.set_mode(0o600);
        std::fs::set_permissions(&pending, permissions).unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&pending)
            .unwrap()
            .set_len(1)
            .unwrap();
        assert!(f.anchor.lock(&f.backend).is_err());

        let g = Fixture::new();
        let mut transaction = g.anchor.lock(&g.backend).unwrap();
        let capture = g.change(&mut transaction, "next\n");
        transaction.commit(&g.backend, &g.writer, &capture).unwrap();
        drop(transaction);
        std::fs::remove_file(g.metadata_path("0000000000000001.json")).unwrap();
        assert!(g.anchor.lock(&g.backend).is_err());
    }

    #[test]
    fn writer_aliases_contend_on_a_fresh_description_and_stale_capture_cannot_publish() {
        let f = Fixture::new();
        let alias = f.writer.try_clone().unwrap();
        let mut transaction = f.anchor.lock(&f.backend).unwrap();
        assert_eq!(
            f.anchor.lock(&f.backend).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        let capture = f.change(&mut transaction, "next\n");
        transaction.commit(&f.backend, &alias, &capture).unwrap();
        drop(transaction);
        let mut other = f.anchor.lock(&f.backend).unwrap();
        assert_eq!(
            other.observe(&f.backend, &alias, &f.name).unwrap(),
            "next\n"
        );
        let later = f.change(&mut other, "last\n");
        other.commit(&f.backend, &alias, &later).unwrap();
        assert!(other.commit(&f.backend, &alias, &capture).is_err());
    }

    #[test]
    fn only_a_durable_retired_tombstone_allows_a_missing_working_record() {
        let f = Fixture::new();
        let mut transaction = f.anchor.lock(&f.backend).unwrap();
        let retired = transaction
            .capture(&f.backend, &f.writer, &f.name, "head\n", true)
            .unwrap();
        transaction.commit(&f.backend, &f.writer, &retired).unwrap();
        assert!(
            transaction
                .capture(&f.backend, &f.writer, &f.name, "head\n", false)
                .is_err()
        );
        drop(transaction);
        std::fs::remove_file(f.record_path()).unwrap();
        f.validate().unwrap();

        let g = Fixture::new();
        std::fs::remove_file(g.record_path()).unwrap();
        assert!(g.validate().is_err());
    }

    fn fill_epoch(f: &Fixture, transaction: &mut Transaction) {
        loop {
            let epoch = transaction.epoch.as_ref().unwrap();
            let ordinal = transaction.latest.as_ref().unwrap().0.ordinal;
            if ordinal - epoch.base.ordinal + 1 >= (epoch::WINDOW - 1) as u64 {
                break;
            }
            transaction.ensure_room(&f.backend, &f.writer).unwrap();
            let capture = f.change(transaction, &format!("record-{ordinal}\n"));
            transaction.commit(&f.backend, &f.writer, &capture).unwrap();
        }
    }

    #[test]
    fn epoch_rotation_bounds_physical_history_without_changing_the_record() {
        let f = Fixture::new();
        for update in 0..256 {
            let mut transaction = f.anchor.lock(&f.backend).unwrap();
            transaction.ensure_room(&f.backend, &f.writer).unwrap();
            let text = format!("record-{update}\n");
            let capture = f.change(&mut transaction, &text);
            transaction.commit(&f.backend, &f.writer, &capture).unwrap();
            assert_eq!(
                transaction.observe(&f.backend, &f.writer, &f.name).unwrap(),
                text
            );
            let directories = std::fs::read_dir(f.root().join(f.anchor.key()))
                .unwrap()
                .filter_map(|entry| {
                    let entry = entry.unwrap();
                    entry
                        .file_name()
                        .to_str()
                        .unwrap()
                        .starts_with("epoch-")
                        .then_some(entry.path())
                })
                .collect::<Vec<_>>();
            assert!(directories.len() <= 2);
            let entries = directories
                .iter()
                .map(|path| std::fs::read_dir(path).unwrap().count())
                .sum::<usize>();
            assert!(
                entries <= epoch::WINDOW * 2,
                "active metadata is bounded after every actual acknowledgement"
            );
        }
    }

    #[test]
    fn a_held_old_epoch_cannot_write_after_a_same_record_rotation() {
        let f = Fixture::new();
        let mut transaction = f.anchor.lock(&f.backend).unwrap();
        fill_epoch(&f, &mut transaction);
        let old = transaction.epoch.take().unwrap();
        let (capture, stamp) = transaction.latest.take().unwrap();
        let file = transaction.tip_file.take().unwrap();
        drop(transaction);
        let mut successor = f.anchor.lock(&f.backend).unwrap();
        successor.ensure_room(&f.backend, &f.writer).unwrap();
        capture.check(&f.backend, &f.writer).unwrap();
        assert_eq!(
            Stamp::read(&file).unwrap(),
            stamp,
            "CONTROL: original metadata remains readable and unchanged"
        );
        assert!(old.recheck(&f.anchor.directory).is_err());
        assert!(
            old.rotate(
                &f.anchor.directory,
                &f.backend,
                &f.writer,
                &capture,
                &file,
                &stamp
            )
            .is_err()
        );
        assert_eq!(
            successor.observe(&f.backend, &f.writer, &f.name).unwrap(),
            capture.text
        );
    }

    #[test]
    fn a_copied_base_cannot_adopt_a_replacement_epoch_directory() {
        let f = Fixture::new();
        let root = f.root().join(f.anchor.key());
        let epoch = root.join("epoch-0000000000000001.noindex");
        let saved = f.root().join("original-epoch");
        let original = std::fs::read(epoch.join("base.json")).unwrap();
        std::fs::rename(&epoch, &saved).unwrap();
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&epoch)
            .unwrap();
        std::fs::copy(saved.join("base.json"), epoch.join("base.json")).unwrap();
        assert_eq!(
            std::fs::read(epoch.join("base.json")).unwrap(),
            original,
            "CONTROL: canonical equal metadata bytes do not bind the new directory"
        );
        assert_eq!(bytes(&f.writer, MAX_ENTRY).unwrap().0, "head\n");
        assert!(f.anchor.lock(&f.backend).is_err());
        assert!(f.anchor.lock_recover(&f.backend).is_err());
    }

    #[test]
    fn complete_rotation_intents_recover_but_unknown_or_restored_origins_do_not() {
        for stage in ["base-covered", "epoch-published", "predecessor-unlinked"] {
            let f = Fixture::new();
            let mut transaction = f.anchor.lock(&f.backend).unwrap();
            fill_epoch(&f, &mut transaction);
            if stage == "predecessor-unlinked" {
                transaction.ensure_room(&f.backend, &f.writer).unwrap();
                fill_epoch(&f, &mut transaction);
            }
            let before = bytes(&f.writer, MAX_ENTRY).unwrap();
            epoch::interrupt(stage);
            assert!(transaction.ensure_room(&f.backend, &f.writer).is_err());
            drop(transaction);
            if stage == "base-covered" {
                assert!(
                    f.anchor.lock(&f.backend).is_err(),
                    "ordinary readers do not publish a prepared epoch"
                );
                let base = File::open(
                    f.root()
                        .join(f.anchor.key())
                        .join("epoch-0000000000000002.preparing.noindex")
                        .join("base.json"),
                )
                .unwrap();
                let origin = Stamp::read(&base).unwrap();
                FAIL_EXACT_FILE_FLUSH.with(|hook| {
                    hook.set(Some((
                        origin.0[0].as_u64().unwrap(),
                        origin.0[1].as_u64().unwrap(),
                    )))
                });
                assert!(
                    f.anchor.lock_recover(&f.backend).is_err(),
                    "cold recovery must cover the exact prepared metadata FILE"
                );
            }
            let mut cold = f.anchor.lock_recover(&f.backend).unwrap();
            assert_eq!(
                cold.observe(&f.backend, &f.writer, &f.name).unwrap(),
                before.0
            );
            assert_eq!(Stamp::read(&f.writer).unwrap(), before.1);
            cold.ensure_room(&f.backend, &f.writer).unwrap();
        }
        for restored in [false, true] {
            let f = Fixture::new();
            let mut transaction = f.anchor.lock(&f.backend).unwrap();
            fill_epoch(&f, &mut transaction);
            epoch::interrupt("base-covered");
            assert!(transaction.ensure_room(&f.backend, &f.writer).is_err());
            drop(transaction);
            if restored {
                let before = bytes(&f.writer, MAX_ENTRY).unwrap();
                past_the_clock_tick(&f.backend, &f.writer);
                f.writer.write_all_at(b"!", 0).unwrap();
                f.writer.write_all_at(before.0.as_bytes(), 0).unwrap();
                assert_ne!(Stamp::read(&f.writer).unwrap(), before.1);
            } else {
                let prepared = f
                    .root()
                    .join(f.anchor.key())
                    .join("epoch-0000000000000002.preparing.noindex");
                std::fs::write(
                    prepared.join("unknown"),
                    b"owned only by the negative control",
                )
                .unwrap();
            }
            assert!(
                f.anchor.lock(&f.backend).is_err(),
                "a newer uncertainty must not fall back to the old epoch"
            );
            assert!(
                f.anchor.lock_recover(&f.backend).is_err(),
                "recovery cannot bless a changed working origin or an unknown preparation member"
            );
        }
    }
}
