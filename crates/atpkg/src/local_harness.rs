// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Explicit local-build admission, separate from signed release authority. This module never
//! runs an executable, changes shims or vendor selection, or enables a harness constructor.

use std::io::{self, Read as _, Seek as _};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::lock::StoreLock;
use crate::store::ToolName;

#[path = "local_harness_fs.rs"]
pub(crate) mod fs;

pub(crate) const SUFFIX: &str = ".local-harness";
const RECEIPT_MAX: usize = 64 << 10;
const RECORD_MAX: usize = 128 << 10;
const BINARY_MAX: u64 = 1 << 30;
const STORE_ENTRIES_MAX: usize = 4096;

/// The caller's explicit opt-in to these local bytes. This is not a signature or proof that
/// an arbitrary receipt was produced by the build tool. No file or environment mints it.
/// The caller decides which source to authorize; this API does not infer Git ancestry.
#[derive(Debug)]
pub struct LocalAuthority {
    program: String,
    command: String,
    source: String,
    target: String,
}

impl LocalAuthority {
    /// Opt in to one harness package, executable command, full source revision and native target.
    /// # Errors
    /// InvalidInput for a non-harness tool, malformed source or nonnative target.
    pub fn new(
        program: &str,
        command: &str,
        expected_source: &str,
        target: &str,
    ) -> io::Result<Self> {
        if ToolName::new(program).is_none()
            || !crate::harness::is_harness(program)
            || ToolName::new(command).is_none()
            || !hex(expected_source, 40)
            || target != crate::cli::current_triple()
            || !crate::manifest::TARGETS.contains(&target)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid local harness scope",
            ));
        }
        Ok(Self {
            program: program.into(),
            command: command.into(),
            source: expected_source.into(),
            target: target.into(),
        })
    }
}

/// What the program's current link must name when selection takes the store lock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExpectedCurrent {
    /// No entry, positively absent (an unreadable or foreign entry is never absence).
    Absent,
    /// This program's own numeric store build.
    Build(u64),
}

/// An admitted local stage, bound to the layout whose lock staged it; fields cannot be forged.
#[derive(Debug)]
pub struct StagedLocal {
    prefix: PathBuf,
    dir: PathBuf,
    record: Record,
}

impl StagedLocal {
    /// The complete candidate's store directory. This grants no execution permission.
    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.dir
    }

    /// The admitted candidate's local identity; it grants no activation authority.
    #[must_use]
    pub fn identity(&self) -> LocalIdentity {
        LocalIdentity::of(&self.record.receipt)
    }
}

/// Metadata read from a fully checked local stage, without execution or release authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalIdentity {
    /// Package identity, distinct from its executable command.
    pub program: String,
    /// Executable command.
    pub command: String,
    /// Full source commit.
    pub source: String,
    /// Full compiler commit.
    pub compiler: String,
    /// Producer-stamped version.
    pub version: String,
    /// Store build number.
    pub build: u64,
    /// Native target.
    pub target: String,
    /// Unverified or repository producer policy.
    pub policy: String,
    /// Development or release profile.
    pub profile: String,
}

impl LocalIdentity {
    fn of(receipt: &Receipt) -> Self {
        Self {
            program: receipt.program.clone(),
            command: receipt.command.clone(),
            source: receipt.source.clone(),
            compiler: receipt.compiler.clone(),
            version: receipt.version.clone(),
            build: receipt.build_number,
            target: receipt.target.clone(),
            policy: receipt.policy.clone(),
            profile: receipt.profile.clone(),
        }
    }
}

/// A positively absent current entry or a complete checked local selection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CurrentLocal {
    /// Missing current link, store directory or program directory under proved ancestors.
    Absent,
    /// Exact local current with coherent ready record, receipt and executable bytes.
    Selected(LocalIdentity),
}

impl CurrentLocal {
    /// The comparison token for a subsequent selection; observe again after an uncertain ACK.
    #[must_use]
    pub const fn expected(&self) -> ExpectedCurrent {
        match self {
            Self::Absent => ExpectedCurrent::Absent,
            Self::Selected(identity) => ExpectedCurrent::Build(identity.build),
        }
    }
}

/// Read current through the held store lock and ancestor descriptors, without changing it.
/// This observation grants no install, execution or signed authority. Foreign, incomplete,
/// malformed and unreadable entries are errors, never absence.
/// # Errors
/// A changed lock/ancestor/current, nonlocal numeric build, invalid receipt, or changed payload.
pub fn observe_current(lock: &StoreLock, program: &str) -> io::Result<CurrentLocal> {
    if ToolName::new(program).is_none() || !crate::harness::is_harness(program) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let layout = lock.checked_layout()?;
    let Some(parent) = fs::Parent::open_optional(lock, program, false)? else {
        return Ok(CurrentLocal::Absent);
    };
    let observed = parent.current(&layout)?;
    let ExpectedCurrent::Build(build) = observed else {
        parent.reprove()?;
        return Ok(CurrentLocal::Absent);
    };
    let record: Record =
        aterm_json::from_slice(&parent.read(&format!("{build}{SUFFIX}"), RECORD_MAX)?)
            .map_err(|e| invalid(format!("local current record: {e}")))?;
    let receipt = &record.receipt;
    // This scope is used only to validate observed metadata; it never escapes as a grant.
    let scope = LocalAuthority::new(program, &receipt.command, &receipt.source, &receipt.target)?;
    if receipt.program != program
        || receipt.build_number != build
        || admit(
            &aterm_json::to_vec(receipt).map_err(|e| invalid(e.to_string()))?,
            &scope,
        )? != *receipt
    {
        return Err(invalid(
            "local current identity differs from its store address",
        ));
    }
    checked(&parent, &layout.build_dir(program, build), &record)?;
    if parent.current(&layout)? != observed {
        return Err(invalid("current changed during observation"));
    }
    parent.reprove()?;
    Ok(CurrentLocal::Selected(LocalIdentity::of(receipt)))
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Executable {
    path: String,
    size: u64,
    sha256: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    schema: u32,
    origin: String,
    program: String,
    source: String,
    build_number: u64,
    version: String,
    source_policy: String,
    command: String,
    compiler: String,
    sysroot: String,
    target: String,
    policy: String,
    profile: String,
    checkout: String,
    logs: String,
    exposes: Vec<String>,
    executables: Vec<Executable>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema: u32,
    receipt: Receipt,
    tree_root: String,
}

fn invalid(why: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why.into())
}
fn hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn version(version: &str, source: &str) -> bool {
    let Some((base, stamp)) = version.split_once("+g") else {
        return false;
    };
    let parts: Vec<_> = base.split('.').collect();
    version.len() <= 128
        && (9..=source.len()).contains(&stamp.len())
        && source.starts_with(stamp)
        && parts.len() == 3
        && parts[2] == "0"
        && parts[..2].iter().all(|s| {
            !s.is_empty()
                && s.bytes().all(|b| b.is_ascii_digit())
                && (s.len() == 1 || !s.starts_with('0'))
                && s.parse::<u64>().is_ok()
        })
}
fn admit(bytes: &[u8], authority: &LocalAuthority) -> io::Result<Receipt> {
    if bytes.len() > RECEIPT_MAX {
        return Err(invalid("local receipt is oversized"));
    }
    let r: Receipt =
        aterm_json::from_slice(bytes).map_err(|e| invalid(format!("local receipt: {e}")))?;
    if r.schema != 2
        || r.origin != "local-build"
        || r.source_policy != "clean-main"
        || r.program != authority.program
        || r.command != authority.command
        || r.source != authority.source
        || r.target != authority.target
        || r.build_number == 0
        || !version(&r.version, &r.source)
        || r.exposes != [r.command.clone()]
        || r.executables.len() != 1
        || !hex(&r.compiler, 40)
        || !matches!(r.policy.as_str(), "repository" | "unverified")
        || !matches!(r.profile.as_str(), "release" | "dev")
        || [&r.sysroot, &r.checkout, &r.logs]
            .iter()
            .any(|s| !Path::new(s).is_absolute() || s.contains('\0'))
    {
        return Err(invalid("local receipt does not match its explicit scope"));
    }
    let exe = &r.executables[0];
    if exe.path != format!("bin/{}", r.command)
        || exe.size == 0
        || exe.size > BINARY_MAX
        || !hex(&exe.sha256, 64)
    {
        return Err(invalid("invalid local executable declaration"));
    }
    Ok(r)
}
fn owned_dir(path: &Path) -> io::Result<()> {
    let file = std::fs::File::from(
        aterm_dirfd::openat(
            aterm_dirfd::CWD,
            path,
            aterm_dirfd::OFlags::RDONLY
                | aterm_dirfd::OFlags::DIRECTORY
                | aterm_dirfd::OFlags::NOFOLLOW
                | aterm_dirfd::OFlags::CLOEXEC,
            aterm_dirfd::Mode::empty(),
        )
        .map_err(io::Error::from)?,
    );
    fs::private(&file)
}
fn receipt_bytes(path: &Path) -> io::Result<Vec<u8>> {
    let mut file = crate::metadata_io::open_regular(path)?;
    if fs::owned_file(&file)?.len() > RECEIPT_MAX as u64 {
        return Err(invalid("local receipt is oversized"));
    }
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut file)
        .take(RECEIPT_MAX as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > RECEIPT_MAX || fs::owned_file(&file)?.len() != bytes.len() as u64 {
        return Err(invalid("local receipt changed while reading"));
    }
    Ok(bytes)
}
fn matches_file(path: &Path, exe: &Executable) -> io::Result<()> {
    matches_open_file(&crate::metadata_io::open_regular(path)?, exe)
}
fn matches_open_file(file: &std::fs::File, exe: &Executable) -> io::Result<()> {
    let mut file = file.try_clone()?;
    file.rewind()?;
    let before = fs::owned_file(&file)?;
    if before.len() != exe.size || before.nlink() != 1 || before.mode() & 0o111 == 0 {
        return Err(invalid(
            "local executable links, owner, mode or size changed",
        ));
    }
    let mut h = aterm_digest::Sha256::new();
    let mut buf = vec![0u8; 64 << 10];
    let mut read = 0u64;
    let mut input = (&mut file).take(exe.size + 1);
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        read += n as u64;
        h.update(&buf[..n]);
    }
    let digest = aterm_codec::hex::encode_lower(&h.finalize());
    if read != exe.size || fs::owned_file(&file)?.len() != exe.size || digest != exe.sha256 {
        return Err(invalid("local executable bytes changed"));
    }
    Ok(())
}
fn stage_error(error: crate::StageError) -> io::Error {
    match error {
        crate::StageError::Io(e) => e,
        other => invalid(other.to_string()),
    }
}
fn only_entry(dir: &std::fs::File, expected: &str) -> io::Result<()> {
    let mut entries = aterm_dirfd::Dir::read_from(dir).map_err(io::Error::from)?;
    let mut found = false;
    // Raw readdir includes the two dot names: one real entry plus end-of-stream is four reads.
    for _ in 0..4 {
        let Some(entry) = entries.next() else {
            return if found {
                Ok(())
            } else {
                Err(invalid("local stage is empty"))
            };
        };
        let entry = entry.map_err(io::Error::from)?;
        let name = entry.file_name().to_bytes();
        if matches!(name, b"." | b"..") {
            continue;
        }
        if found || name != expected.as_bytes() {
            return Err(invalid("local stage contains an unexpected entry"));
        }
        found = true;
    }
    Err(invalid("local stage inventory exceeds its bounded read"))
}
fn checked(parent: &fs::Parent, dir: &Path, record: &Record) -> io::Result<()> {
    let name = dir.file_name().ok_or(io::ErrorKind::InvalidInput)?;
    let root = fs::directory(parent.held(), name)?;
    let bin = fs::directory(&root, std::ffi::OsStr::new("bin"))?;
    fs::private(&root)?;
    fs::private(&bin)?;
    only_entry(&root, "bin")?;
    only_entry(&bin, &record.receipt.command)?;
    let build = record.receipt.build_number;
    let published: Record =
        aterm_json::from_slice(&parent.read(&format!("{build}{SUFFIX}"), RECORD_MAX)?)
            .map_err(|e| invalid(e.to_string()))?;
    if &published != record || record.schema != 1 {
        return Err(invalid("local receipt changed"));
    }
    let exe = &record.receipt.executables[0];
    let file = fs::regular(&bin, std::ffi::OsStr::new(&record.receipt.command))?;
    let expected_root = crate::tree::root_of_entry_lines(vec![crate::tree::entry_line(
        exe.path.as_bytes(),
        0o755,
        &exe.sha256,
    )]);
    let ready =
        crate::store::ready_body_with_inventory(Some(&format!("bin:{}", record.receipt.command)));
    if parent.read(&format!("{build}.ready"), 4096)? != ready.as_bytes()
        || !hex(&record.tree_root, 64)
        || expected_root != record.tree_root
        || file.metadata()?.mode() & 0o7777 != 0o755
        || parent.read(&format!("{build}{}", crate::store::VERSION_SUFFIX), 1024)?
            != format!(
                "{}\n{}\n",
                crate::store::VERSION_HEADER,
                record.receipt.version
            )
            .as_bytes()
        || parent.read(&format!("{build}{}", crate::store::EXPOSES_SUFFIX), 1024)?
            != format!(
                "{}\n{}\n",
                crate::store::EXPOSES_HEADER,
                record.receipt.command
            )
            .as_bytes()
    {
        return Err(invalid("local stage metadata or readiness changed"));
    }
    matches_open_file(&file, exe)?;
    parent.reprove()
}

/// Admit the producer's `local-build.json` and single `bin/<command>` into its package's store.
/// An exact complete existing candidate is reused; all other collisions are preserved.
/// # Errors
/// Malformed, mismatched, linked or changed inputs; an occupied name, uncertain metadata,
/// insufficient permissions, or any staging/durability error. No current link is changed.
pub fn stage(
    lock: &StoreLock,
    authority: &LocalAuthority,
    artifact_dir: &Path,
) -> io::Result<StagedLocal> {
    if !artifact_dir.is_absolute() || artifact_dir.canonicalize()? != artifact_dir {
        return Err(invalid(
            "local artifact directory must be exact and canonical",
        ));
    }
    owned_dir(artifact_dir)?;
    owned_dir(&artifact_dir.join("bin"))?;
    let receipt = admit(
        &receipt_bytes(&artifact_dir.join("local-build.json"))?,
        authority,
    )?;
    let input_path = artifact_dir.join(&receipt.executables[0].path);
    matches_file(&input_path, &receipt.executables[0])?;
    let input = crate::metadata_io::open_regular(&input_path)?;
    fs::owned_file(&input)?;
    let layout = lock.checked_layout()?;
    let parent = fs::Parent::open(lock, &receipt.program, true)?;
    let dir = layout.build_dir(&receipt.program, receipt.build_number);
    match aterm_dirfd::fstatat(
        parent.held(),
        dir.file_name().unwrap(),
        aterm_dirfd::AtFlags::SYMLINK_NOFOLLOW,
    )
    .map_err(io::Error::from)
    {
        Ok(_) => {
            let old: Record = aterm_json::from_slice(
                &parent.read(&format!("{}{SUFFIX}", receipt.build_number), RECORD_MAX)?,
            )
            .map_err(|e| invalid(e.to_string()))?;
            if old.schema != 1 || old.receipt != receipt {
                return Err(io::ErrorKind::AlreadyExists.into());
            }
            checked(&parent, &dir, &old)?;
            return Ok(StagedLocal {
                prefix: layout.prefix,
                dir,
                record: old,
            });
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    // Orphan sidecars and earlier attempts belong to their first publisher, not this retry.
    let prefix = format!("{}.", receipt.build_number);
    for (seen, entry) in aterm_dirfd::Dir::read_from(parent.held())
        .map_err(io::Error::from)?
        .enumerate()
    {
        if seen == STORE_ENTRIES_MAX {
            return Err(invalid("local store inventory exceeds its bounded scan"));
        }
        if entry
            .map_err(io::Error::from)?
            .file_name()
            .to_bytes()
            .starts_with(prefix.as_bytes())
        {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
    }
    let artifact = crate::Artifact {
        target: receipt.target.clone(),
        kind: "binary".into(),
        protocol: "local".into(),
        asset: String::new(),
        sha256: receipt.executables[0].sha256.clone(),
        sha512: String::new(),
        tree_root: String::new(),
        size: receipt.executables[0].size,
        reloc: String::new(),
        cost: crate::Cost {
            disk_installed: receipt.executables[0].size,
            ..Default::default()
        },
        url: String::new(),
        payload: "raw-binary".into(),
        entry: receipt.command.clone(),
        strip_components: 0,
        links: Default::default(),
        vendor: String::new(),
        provenance: None,
    };
    let verify = |_: &Path| {
        lock.checked_layout().map_err(crate::StageError::Io)?;
        parent.reprove().map_err(crate::StageError::Io)?;
        Ok(())
    };
    let sidecars = |root: &str| {
        let record = Record {
            schema: 1,
            receipt: receipt.clone(),
            tree_root: root.into(),
        };
        let bytes =
            aterm_json::to_vec(&record).map_err(|e| crate::StageError::Payload(e.to_string()))?;
        if bytes.len() > RECORD_MAX {
            return Err(crate::StageError::Payload(
                "local metadata is oversized".into(),
            ));
        }
        Ok(vec![
            (SUFFIX, bytes),
            (
                crate::store::VERSION_SUFFIX,
                format!("{}\n{}\n", crate::store::VERSION_HEADER, receipt.version).into_bytes(),
            ),
            (
                crate::store::EXPOSES_SUFFIX,
                format!("{}\n{}\n", crate::store::EXPOSES_HEADER, receipt.command).into_bytes(),
            ),
        ])
    };
    let hooks = crate::install::StageHooks {
        pre_swap: Some(&verify),
        sidecars: Some(&sidecars),
        ..crate::install::StageHooks::NONE
    };
    let root = crate::install::stage_local_binary(&parent, &artifact, input, &dir, &hooks)
        .map_err(stage_error)?;
    parent.sync()?;
    let record = Record {
        schema: 1,
        receipt,
        tree_root: root,
    };
    checked(&parent, &dir, &record)?;
    Ok(StagedLocal {
        prefix: layout.prefix,
        dir,
        record,
    })
}

/// Select this validated candidate by changing only its program's current link.
/// # Errors
/// A different store lock, changed candidate, dev link, unknown/foreign current entry, stale
/// expected current, or atomic-link failure. This never installs or refreshes any shim.
/// An error after the atomic rename may leave the candidate selected; observe current before retrying.
pub fn select_current(
    lock: &StoreLock,
    staged: &StagedLocal,
    expected: ExpectedCurrent,
) -> io::Result<()> {
    let layout = lock.checked_layout()?;
    if layout.prefix != staged.prefix {
        return Err(invalid("candidate belongs to another store"));
    }
    let program = &staged.record.receipt.program;
    let parent = fs::Parent::open(lock, program, false)?;
    checked(&parent, &staged.dir, &staged.record)?;
    match std::fs::symlink_metadata(layout.link_marker(program)) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        _ => {
            return Err(invalid(
                "local current selection refuses an existing or unknown dev link",
            ));
        }
    }
    let observed = parent.current(&layout)?;
    if observed != expected {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "current changed since observation",
        ));
    }
    fs::checkpoint("select");
    parent.select(&staged.dir)
}

#[cfg(test)]
#[path = "local_harness_tests.rs"]
mod tests;
