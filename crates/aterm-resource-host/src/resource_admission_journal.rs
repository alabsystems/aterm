// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Private attempt records precede CPU publication. Completed attempt names
//! remain durable replay tombstones; startup scans only outstanding records.
use super::*;
use aterm_json::Value;
#[cfg(test)]
use std::os::unix::fs::DirBuilderExt as _;
use std::{
    io::{Read as _, Write as _},
    os::unix::fs::{MetadataExt as _, OpenOptionsExt as _},
};

const DIRECTORY: &str = "resource-host-v3.noindex";
const MAX_RECORD: u64 = 16384;
fn storage(namespace: &Namespace) -> io::Result<PathBuf> {
    namespace.validate()?;
    let backend = namespace.backend();
    // The cohort backend has a closed, independently audited record layout.
    // Host recovery is a sibling inside the same private, inode-bound namespace,
    // never an exception to its unknown-child refusal.
    Ok(backend
        .parent()
        .ok_or_else(|| refused("memory namespace container unavailable"))?
        .join(DIRECTORY))
}
pub(super) struct Journal {
    namespace: Namespace,
    root: PathBuf,
    active: PathBuf,
    done: PathBuf,
    authority: PathBuf,
    identity: (u64, u64),
    debt: ExternalDebt,
    readiness: Value,
    faults: std::sync::Mutex<std::collections::BTreeMap<String, String>>,
    #[cfg(all(test, target_os = "macos"))]
    lose_running_ack: std::sync::atomic::AtomicBool,
}
fn private(path: &std::path::Path, directory: bool) -> io::Result<File> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(
            libc::O_NOFOLLOW
                | libc::O_CLOEXEC
                | libc::O_NONBLOCK
                | if directory { libc::O_DIRECTORY } else { 0 },
        )
        .open(path)?;
    let metadata = file.metadata()?;
    if metadata.uid() != aterm_uds::peer::our_uid()
        || metadata.mode() & 0o077 != 0
        || (directory && !metadata.is_dir())
        || (!directory && (!metadata.is_file() || metadata.nlink() != 1))
    {
        return Err(refused("host journal is not private original storage"));
    }
    Ok(file)
}
fn identity(file: &File) -> io::Result<(u64, u64)> {
    let m = file.metadata()?;
    Ok((m.dev(), m.ino()))
}

#[derive(PartialEq, Eq)]
struct RecordStamp {
    identity: (u64, u64),
    length: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}
impl RecordStamp {
    fn read(file: &File) -> io::Result<Self> {
        let metadata = file.metadata()?;
        Ok(Self {
            identity: (metadata.dev(), metadata.ino()),
            length: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        })
    }
    fn check(&self, file: &File, path: &std::path::Path) -> io::Result<()> {
        if Self::read(file)? != *self || Self::read(&private(path, false)?)? != *self {
            return Err(refused(
                "host terminal record changed during acknowledgment",
            ));
        }
        Ok(())
    }
}

fn exists(path: &std::path::Path) -> io::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}
fn name(binding: &Binding) -> io::Result<String> {
    let suffix = binding
        .session
        .strip_prefix("s-")
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 62
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .ok_or_else(|| refused("invalid host attempt session"))?;
    Ok(format!(
        "s-{suffix}.{}.json",
        aterm_codec::hex::encode_lower(&binding.attempt)
    ))
}
impl Journal {
    pub(super) fn open(namespace: &Namespace, debt: &ExternalDebt) -> io::Result<Self> {
        namespace.validate()?;
        let root = storage(namespace)?;
        let directory = private(&root, true)?;
        let text = read(&root.join("ready"))?;
        let expected = readiness(namespace, &root, debt)?;
        if text != expected {
            return Err(refused("host journal readiness changed"));
        }
        let active = root.join("active");
        let done = root.join("done");
        let authority = root.join("authority");
        for path in [&active, &done, &authority] {
            private(path, true)?;
        }
        Ok(Self {
            namespace: namespace.clone(),
            root,
            active,
            done,
            authority,
            identity: identity(&directory)?,
            debt: debt.clone(),
            readiness: expected,
            faults: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            #[cfg(all(test, target_os = "macos"))]
            lose_running_ack: std::sync::atomic::AtomicBool::new(false),
        })
    }
    /// Called only while preparing a fresh, unadopted namespace. Runtime open
    /// deliberately cannot recreate deleted or partial host recovery storage.
    #[cfg(test)]
    pub(super) fn prepare(namespace: &Namespace, debt: &ExternalDebt) -> io::Result<()> {
        namespace.validate()?;
        let root = storage(namespace)?;
        std::fs::DirBuilder::new().mode(0o700).create(&root)?;
        for name in ["active", "done", "authority"] {
            let path = root.join(name);
            std::fs::DirBuilder::new().mode(0o700).create(&path)?;
            aterm_durable::durable(&private(&path, true)?)?;
        }
        let directory = private(&root, true)?;
        let value = readiness(namespace, &root, debt)?;
        write_new(&root.join("ready"), &value)?;
        aterm_durable::durable(&directory)?;
        aterm_durable::durable(&private(
            root.parent()
                .ok_or_else(|| refused("host journal parent unavailable"))?,
            true,
        )?)
    }
    fn validate(&self) -> io::Result<()> {
        self.namespace.validate()?;
        if identity(&private(&self.root, true)?)? != self.identity {
            return Err(refused("host journal directory changed"));
        }
        if readiness(&self.namespace, &self.root, &self.debt)? != self.readiness
            || read(&self.root.join("ready"))? != self.readiness
        {
            return Err(refused("host CPU/memory recovery binding changed"));
        }
        Ok(())
    }
    pub(super) fn fresh(&self, binding: &Binding) -> io::Result<bool> {
        self.validate()?;
        let name = name(binding)?;
        for path in [self.active.join(&name), self.done.join(&name)] {
            match std::fs::symlink_metadata(path) {
                Ok(_) => return Ok(false),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        Ok(true)
    }
    /// Failure data never authorizes recovery or settlement. Retain the first
    /// observation at each site for this owner, including when damaged storage
    /// cannot record it.
    pub(super) fn fault(&self, faulted: &mut bool, site: &str, detail: impl std::fmt::Debug) {
        *faulted = true;
        self.record_fault(site, detail);
    }
    pub(super) fn record_fault(&self, site: &str, detail: impl std::fmt::Debug) {
        let mut faults = self
            .faults
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if faults.contains_key(site) {
            return;
        }
        let detail: String = format!("{detail:?}").chars().take(4096).collect();
        faults.insert(site.into(), detail.clone());
        let record = aterm_json::json!({"version":1,"owner_pid":std::process::id(),"site":site,"detail":detail});
        let result: io::Result<()> = (|| {
            self.validate()?;
            let nonce = aterm_uds::rand::hex_token::<16>()?;
            let stage = self.root.join(format!(".fault-{nonce}"));
            write_new(&stage, &record)?;
            // A later owner must not overwrite the evidence from this one.
            std::fs::rename(stage, self.root.join(format!("fault-{site}-{nonce}.json")))?;
            aterm_durable::durable(&private(&self.root, true)?)
        })();
        if let Err(error) = result {
            faults.insert("diagnostic-storage".into(), format!("{error:?}"));
        }
    }
    pub(super) fn fault_summary(&self) -> String {
        format!(
            "{:?}",
            self.faults
                .lock()
                .unwrap_or_else(|error| error.into_inner())
        )
    }
    pub(super) fn create(&self, binding: &Binding) -> io::Result<()> {
        if !self.fresh(binding)? {
            return Err(refused("host attempt was already submitted"));
        }
        write_new(
            &self.active.join(name(binding)?),
            &aterm_json::json!({"version":3,"session":binding.session,"attempt":aterm_codec::hex::encode_lower(&binding.attempt),"phase":"queued"}),
        )?;
        aterm_durable::durable(&private(&self.active, true)?)
    }
    pub(super) fn update(&self, binding: &Binding, phase: &str, fields: Value) -> io::Result<()> {
        self.edit(binding, Some(phase), fields)
    }
    #[cfg(all(test, target_os = "macos"))]
    pub(super) fn lose_running_ack_once(&self) {
        self.lose_running_ack
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
    /// Update retained bookkeeping without resurrecting the native life phase.
    pub(super) fn patch(&self, binding: &Binding, fields: Value) -> io::Result<()> {
        self.edit(binding, None, fields)
    }
    fn edit(&self, binding: &Binding, phase: Option<&str>, fields: Value) -> io::Result<()> {
        self.validate()?;
        let path = self.active.join(name(binding)?);
        let mut record = read(&path)?;
        check_binding(&record, binding)?;
        let object = record
            .as_object_mut()
            .ok_or_else(|| refused("invalid attempt record"))?;
        if let Some(phase) = phase {
            object.insert("phase".into(), Value::String(phase.into()));
        }
        for (key, value) in fields
            .as_object()
            .ok_or_else(|| refused("invalid attempt update"))?
        {
            if ["version", "session", "attempt", "phase"].contains(&key.as_str()) {
                return Err(refused("attempt update changes identity"));
            }
            object.insert(key.clone(), value.clone());
        }
        let stage = self
            .active
            .join(format!(".stage-{}", aterm_uds::rand::hex_token::<16>()?));
        write_new(&stage, &record)?;
        std::fs::rename(stage, &path)?;
        aterm_durable::durable(&private(&self.active, true)?)?;
        #[cfg(all(test, target_os = "macos"))]
        if phase == Some("running")
            && self
                .lose_running_ack
                .swap(false, std::sync::atomic::Ordering::Relaxed)
        {
            return Err(io::Error::other(
                "controlled Root running commit acknowledgement lost",
            ));
        }
        Ok(())
    }
    /// Publish only the attempt's durable bookkeeping after the caller has already proved
    /// actual resource closure or that no ARM occurred. A tombstone is never CPU/memory
    /// settlement authority. Repeating this after an unacknowledged rename is safe.
    pub(super) fn settle(&self, binding: &Binding) -> io::Result<()> {
        self.validate()?;
        let name = name(binding)?;
        match (
            exists(&self.active.join(&name))?,
            exists(&self.done.join(&name))?,
        ) {
            (true, false) => self.update(binding, "settled", aterm_json::json!({}))?,
            (false, true) => {}
            _ => {
                return Err(refused(
                    "host settlement needs exactly one original attempt record",
                ));
            }
        }
        self.finish(binding)
    }

    /// Finish a settled record, including an exact done-only retry after failed directory
    /// coverage. This acknowledges journal durability only, not resource closure.
    pub(super) fn finish(&self, binding: &Binding) -> io::Result<()> {
        self.validate()?;
        let name = name(binding)?;
        let active = self.active.join(&name);
        let done = self.done.join(&name);
        let moving = match (exists(&active)?, exists(&done)?) {
            (true, false) => true,
            (false, true) => false,
            _ => {
                return Err(refused(
                    "host finish needs exactly one original attempt record",
                ));
            }
        };
        let path = if moving { &active } else { &done };
        let file = private(path, false)?;
        let before = RecordStamp::read(&file)?;
        let record = read_held(&file)?;
        check_binding(&record, binding)?;
        if record["phase"].as_str() != Some("settled") {
            return Err(refused(
                "unsettled attempt cannot become a replay tombstone",
            ));
        }
        before.check(&file, path)?;
        if moving {
            if exists(&done)? {
                return Err(refused("attempt tombstone appeared before finish"));
            }
            std::fs::rename(&active, &done)?;
        }
        // Our known rename can change ctime, never the inode, bytes or data mtime. Freeze
        // that transition before any covering I/O, then hold the SAME descriptor to the end.
        let renamed = RecordStamp::read(&file)?;
        if before.identity != renamed.identity
            || before.length != renamed.length
            || before.modified != renamed.modified
            || (!moving && before != renamed)
        {
            return Err(refused("host terminal rename changed its original record"));
        }
        renamed.check(&file, &done)?;
        aterm_durable::durable(&file)?;
        aterm_durable::durable(&private(&self.active, true)?)?;
        aterm_durable::durable(&private(&self.done, true)?)?;
        self.validate()?;
        if exists(&active)? {
            return Err(refused(
                "active attempt reappeared during terminal acknowledgment",
            ));
        }
        renamed.check(&file, &done)
    }
    pub(super) fn authority(&self, binding: &Binding) -> io::Result<(PathBuf, String)> {
        self.validate()?;
        Ok((
            self.authority.clone(),
            name(binding)?.replace(".json", ".cap"),
        ))
    }
    pub(super) fn outstanding(&self) -> io::Result<Vec<Value>> {
        self.validate()?;
        let mut records = Vec::new();
        for entry in std::fs::read_dir(&self.active)? {
            let path = entry?.path();
            if path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(".stage-"))
            {
                return Err(refused("host attempt publication is incomplete"));
            }
            if records.len() >= aterm_capacity::host_protocol::MAX_JOBS {
                return Err(refused("too many outstanding host attempts"));
            }
            let record = read(&path)?;
            if record["version"].as_u64() != Some(3) {
                return Err(refused("unknown host attempt record"));
            }
            let binding = record_binding(&record)?;
            if path.file_name() != Some(std::ffi::OsStr::new(&name(&binding)?)) {
                return Err(refused(
                    "host record filename differs from its original attempt",
                ));
            }
            records.push(record);
        }
        Ok(records)
    }
}
fn check_binding(record: &Value, binding: &Binding) -> io::Result<()> {
    if record["version"].as_u64() != Some(3)
        || record["session"].as_str() != Some(binding.session.as_str())
        || record["attempt"].as_str()
            != Some(aterm_codec::hex::encode_lower(&binding.attempt).as_str())
    {
        return Err(refused("host attempt identity changed"));
    }
    Ok(())
}
fn read(path: &std::path::Path) -> io::Result<Value> {
    let mut text = String::new();
    private(path, false)?
        .take(MAX_RECORD + 1)
        .read_to_string(&mut text)?;
    if text.len() as u64 > MAX_RECORD {
        return Err(refused("oversized host attempt record"));
    }
    aterm_json::from_str(&text).map_err(|_| refused("invalid host attempt record"))
}

fn read_held(file: &File) -> io::Result<Value> {
    use std::os::unix::fs::FileExt as _;
    let mut bytes = vec![0; MAX_RECORD as usize + 1];
    let mut length = 0;
    while length < bytes.len() {
        match file.read_at(&mut bytes[length..], length as u64) {
            Ok(0) => break,
            Ok(read) => length += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    if length as u64 > MAX_RECORD {
        return Err(refused("oversized host terminal record"));
    }
    aterm_json::from_slice(&bytes[..length]).map_err(|_| refused("invalid host terminal record"))
}
fn write_new(path: &std::path::Path, value: &Value) -> io::Result<()> {
    let text = value.to_string();
    if text.len() as u64 > MAX_RECORD {
        return Err(refused("oversized host attempt record"));
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    if identity(&file)? != identity(&private(path, false)?)? {
        return Err(refused("host attempt file changed"));
    }
    file.write_all(text.as_bytes())?;
    aterm_durable::durable(&file)
}

pub(super) fn record_binding(record: &Value) -> io::Result<Binding> {
    let session = record["session"]
        .as_str()
        .ok_or_else(|| refused("missing original session"))?
        .to_owned();
    let hex = record["attempt"]
        .as_str()
        .filter(|text| aterm_codec::hex::is_lower(text, 32))
        .ok_or_else(|| refused("invalid original attempt"))?;
    let mut attempt = [0; 16];
    if !aterm_codec::hex::decode_into(hex.as_bytes(), &mut attempt) {
        return Err(refused("invalid original attempt"));
    }
    let binding = Binding { session, attempt };
    name(&binding)?;
    check_binding(record, &binding)?;
    Ok(binding)
}

fn readiness(
    namespace: &Namespace,
    root: &std::path::Path,
    debt: &ExternalDebt,
) -> io::Result<Value> {
    let (native_dev, native_ino) = debt.native_manifest_identity()?;
    let (debt_dev, debt_ino) = identity(&private(debt.path(), true)?)?;
    // Validate the sidecar after reading its original root identity. A replaced
    // domain cannot become a new empty supply under an old host ready record.
    debt.reserved()?;
    let node = |path: &std::path::Path| -> io::Result<Value> {
        let (device, inode) = identity(&private(path, true)?)?;
        Ok(aterm_json::json!({"device":device,"inode":inode}))
    };
    Ok(
        aterm_json::json!({"version":3,"memory_backend":node(&namespace.backend())?,"root":node(root)?,
        "native_manifest":{"device":native_dev,"inode":native_ino},
        "retained_root":{"device":debt_dev,"inode":debt_ino},
        "active":node(&root.join("active"))?,"done":node(&root.join("done"))?,"authority":node(&root.join("authority"))?}),
    )
}

#[cfg(test)]
#[path = "resource_admission_journal/tests.rs"]
mod tests;
