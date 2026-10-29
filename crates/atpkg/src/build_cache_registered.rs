// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Explicit Cargo caches created by aterm, at the caller's chosen location.
//!
//! A name or a Cargo tag is not ownership. Only an atomic directory creation, followed by a
//! receipt binding its filesystem identity, admits a cache. Existing unrecorded targets stay
//! unmanaged and keep their output paths. The index avoids searching the person's folders.

use std::path::{Path, PathBuf};

/// The internal front-door call a managed compiler shim makes before it execs Cargo.
pub const REGISTER_VERB: &str = "__build_cache_register";

/// Capability response for external producers before they create an explicit target.
pub const REGISTER_CAPABILITY: &str = "aterm-build-cache-register-v1";

/// Where the bounded receipts live, relative to the user's home.
pub const INDEX: &str = "Library/Caches/aterm/.ubd/registered-builds.noindex";

const MARKER: &str = ".aterm-build-cache.json";
const MAX_RECORD: usize = 64 << 10;
const MAX_ROOTS: usize = 4096;
const MAX_OWNERS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Identity {
    device: u64,
    inode: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Receipt {
    schema: u32,
    root: PathBuf,
    identity: Identity,
    nonce: String,
    owners: Vec<PathBuf>,
    /// A bounded owner list that overflowed can never authorize whole-cache retirement.
    owners_unknown: bool,
    #[serde(default)]
    pending: Option<Pending>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Pending {
    directory: PathBuf,
    identity: Identity,
    /// Set durably only after the staged payload matched the admitted root. A later pass
    /// may then finish a partially removed payload whose marker has already gone.
    #[serde(default)]
    authorized: bool,
}

/// A cache whose creation receipt still names the same private directory and marker.
#[derive(Clone, Debug)]
pub struct Registered {
    pub root: PathBuf,
    pub owners: Vec<PathBuf>,
    pub owners_unknown: bool,
    receipt_path: PathBuf,
    identity: Identity,
    nonce: String,
}

impl Registered {
    /// Whether this still names the same active cache and owners under its creation receipt.
    /// Retired stages and a replaced root are not current, even when [`roots`] retains their
    /// receipts so the reclaimer can finish them. This read does not authorize a removal.
    #[must_use]
    pub fn is_current(&self, home: &Path) -> bool {
        read(&self.receipt_path).is_some_and(|receipt| {
            receipt.root == self.root
                && receipt.identity == self.identity
                && receipt.nonce == self.nonce
                && receipt.owners == self.owners
                && receipt.owners_unknown == self.owners_unknown
                && receipt.pending.is_none()
                && same(home, &receipt)
        })
    }
}

#[cfg(unix)]
fn identity(path: &Path) -> Option<Identity> {
    use std::os::unix::fs::MetadataExt as _;
    let metadata = std::fs::symlink_metadata(path).ok()?;
    // SAFETY: geteuid has no preconditions and always returns the current effective uid.
    let uid = unsafe { libc::geteuid() };
    (metadata.is_dir() && metadata.uid() == uid && metadata.mode() & 0o022 == 0).then(|| Identity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(not(unix))]
fn identity(_: &Path) -> Option<Identity> {
    None
}

fn scope(home: &Path, path: &Path) -> bool {
    path.is_absolute()
        && path != home
        && path.starts_with(home)
        && !path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        && super::checkout::never_looked_at(path, Some(home)).is_none()
        && !crate::protected::under_app_data(home, path)
        && !crate::protected::under_file_provider_domain(home, path)
}

/// Create the fixed index without following a directory symlink or changing existing modes.
#[cfg(unix)]
fn index(home: &Path) -> Option<PathBuf> {
    let mut at = home.to_path_buf();
    for component in Path::new(INDEX).components() {
        at.push(component);
        match std::fs::symlink_metadata(&at) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::DirBuilderExt as _;
                    std::fs::DirBuilder::new().mode(0o700).create(&at).ok()?;
                }
                #[cfg(not(unix))]
                return None;
            }
            Err(_) => return None,
        }
        identity(&at)?;
    }
    let gates = at.join("locks.noindex");
    if !gates.exists() {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new().mode(0o700).create(&gates).ok()?;
    }
    identity(&gates)?;
    Some(at)
}

fn gate_path(receipt: &Path) -> Option<PathBuf> {
    Some(
        receipt
            .parent()?
            .join("locks.noindex")
            .join(receipt.file_name()?)
            .with_extension("lock"),
    )
}

/// Called under the admission lock. Never unlink a gate another process might already have
/// opened: bound retained gates too, and leave further fresh paths unmanaged at that limit.
#[cfg(unix)]
fn open_gate(path: &Path) -> Option<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    match std::fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if std::fs::read_dir(path.parent()?)
                .ok()?
                .take(MAX_ROOTS)
                .count()
                >= MAX_ROOTS
            {
                return None;
            }
        }
        Err(_) => return None,
    }
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .ok()
}

#[cfg(unix)]
fn key(path: &Path) -> Option<String> {
    Some(aterm_codec::hex::encode_lower(
        &aterm_digest::Sha256::digest(path.to_str()?.as_bytes()),
    ))
}

fn read(path: &Path) -> Option<Receipt> {
    let bytes = crate::metadata_io::read_bounded_regular(path, MAX_RECORD).ok()?;
    let receipt: Receipt = aterm_json::from_slice(&bytes).ok()?;
    (receipt.schema == 1
        && receipt.owners.len() <= MAX_OWNERS
        && receipt.nonce.len() == 32
        && receipt.nonce.bytes().all(|byte| byte.is_ascii_hexdigit()))
    .then_some(receipt)
}

fn same(home: &Path, receipt: &Receipt) -> bool {
    scope(home, &receipt.root)
        && std::fs::canonicalize(&receipt.root).ok().as_ref() == Some(&receipt.root)
        && identity(&receipt.root).as_ref() == Some(&receipt.identity)
        && [".git", "Cargo.toml", "Targo.toml"].iter().all(|name| {
            std::fs::symlink_metadata(receipt.root.join(name))
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        })
        && read(&receipt.root.join(MARKER)).is_some_and(|marker| {
            marker.root == receipt.root
                && marker.identity == receipt.identity
                && marker.nonce == receipt.nonce
        })
}

#[cfg(unix)]
fn write(path: &Path, receipt: &Receipt) -> Option<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let parent = path.parent()?;
    let stage = aterm_tempfile::Builder::new()
        .prefix("cache-receipt-")
        .tempdir_in(parent)
        .ok()?;
    let pending = stage.path().join("receipt");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&pending)
        .ok()?;
    let bytes = aterm_json::to_vec(receipt).ok()?;
    if bytes.len() > MAX_RECORD {
        return None;
    }
    file.write_all(&bytes).ok()?;
    aterm_durable::ordered(&file).ok()?;
    std::fs::rename(pending, path).ok()?;
    aterm_durable::ordered_dir(parent).ok()
}

#[cfg(unix)]
fn pending_copy(index: &Path, root_key: &str, receipt: &Receipt) -> Option<()> {
    let pending = receipt.pending.as_ref()?;
    let stage_key = key(&pending.directory)?;
    let path = index.join(format!(
        "{root_key}-{}-{stage_key}.retired.json",
        receipt.nonce
    ));
    if let Some(before) = read(&path) {
        return (before.nonce == receipt.nonce
            && before.identity == receipt.identity
            && before.pending.as_ref().is_some_and(|before| {
                before.directory == pending.directory && before.identity == pending.identity
            }))
        .then_some(());
    }
    if std::fs::symlink_metadata(&path).is_ok() || record_count(index)? >= MAX_ROOTS {
        return None;
    }
    // A separate stage receipt survives publishing a replacement at the original target.
    // Its own gate exists before the receipt can become visible to a reclaimer.
    let file = open_gate(&gate_path(&path)?)?;
    let _held = crate::lock::Flock::try_lock(file).ok()?;
    write(&path, receipt)
}

#[cfg(unix)]
fn record_count(index: &Path) -> Option<usize> {
    let mut count = 0;
    for entry in std::fs::read_dir(index).ok()? {
        let entry = entry.ok()?;
        if entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            count += 1;
            if count >= MAX_ROOTS {
                break;
            }
        }
    }
    Some(count)
}

/// Contained producers need no new write authority once their owner was published outside
/// the sandbox. The shared gate excludes retirement while the existing receipt is read.
#[cfg(unix)]
fn already_recorded(home: &Path, root: &Path, workspace: &Path) -> bool {
    let directory = home.join(INDEX);
    let gates = directory.join("locks.noindex");
    if identity(&directory).is_none()
        || identity(&gates).is_none()
        || std::fs::canonicalize(&gates).ok().as_ref() != Some(&gates)
    {
        return false;
    }
    let Some(key) = key(root) else {
        return false;
    };
    let path = directory.join(format!("{key}.json"));
    let Some(gate) = gate_path(&path) else {
        return false;
    };
    let Ok(file) = crate::metadata_io::open_regular(&gate) else {
        return false;
    };
    let Ok(_held) = crate::lock::Flock::try_lock_shared(file) else {
        return false;
    };
    read(&path).is_some_and(|receipt| {
        receipt.root == root
            && receipt.pending.is_none()
            && same(home, &receipt)
            && (receipt.owners_unknown || receipt.owners.iter().any(|owner| owner == workspace))
    })
}

/// A producer must not continue on a previously owned cache if its owner cannot be recorded.
/// Unmanaged existing paths are a successful observation with `false`, never an adoption.
/// A fresh atomic creation is the only way to begin ownership; no existing output moves.
///
/// # Errors
/// The registration gate was busy or an admitted cache's owner could not be published.
#[cfg(unix)]
pub fn create_checked(home: &Path, root: &Path, workspace: &Path) -> std::io::Result<bool> {
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
    let mut must_record = false;
    let mut attempt = || -> Option<()> {
        if !scope(home, root) || !scope(home, workspace) {
            return None;
        }
        // An existing ownership marker makes publication mandatory even when the index
        // itself became unreadable. Unknown ordinary targets have no such declaration.
        let declared = match std::fs::symlink_metadata(root.join(MARKER)) {
            Ok(_) => true,
            Err(e) => e.kind() != std::io::ErrorKind::NotFound,
        };
        must_record = declared;
        let parent = root.parent()?;
        if std::fs::canonicalize(parent).ok().as_deref() != Some(parent)
            || std::fs::canonicalize(workspace).ok().as_deref() != Some(workspace)
            || !super::checkout::MANIFESTS.iter().any(|name| {
                std::fs::symlink_metadata(workspace.join(name))
                    .is_ok_and(|metadata| metadata.is_file())
            })
        {
            return None;
        }
        if already_recorded(home, root, workspace) {
            return Some(());
        }
        let index = index(home)?;
        // Serialize admission and archived-stage publication as well as each root. Counting
        // only JSON records bounds the index even when several independent builds start.
        let admission = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(index.join("locks.noindex/admission.lock"))
            .ok()?;
        let _admission = crate::lock::Flock::lock(admission).ok()?;
        let key = key(root)?;
        let path = index.join(format!("{key}.json"));
        let lock_path = gate_path(&path)?;
        let lock = open_gate(&lock_path)?;
        must_record = true;
        // The record may be between fresh creation and publication, so a busy gate is never
        // permission to use the directory without recording this producer's owner.
        let _held = crate::lock::Flock::try_lock(lock).ok()?;
        must_record = declared;
        let before = read(&path);
        match std::fs::symlink_metadata(&path) {
            Ok(_) if before.is_none() => {
                must_record = true;
                return None;
            }
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                must_record = true;
                return None;
            }
            _ => {}
        }
        if before.as_ref().is_some_and(|before| before.root != root) {
            must_record = true;
            return None;
        }
        if let Some(before) = before
            && before.pending.is_some()
        {
            must_record = true;
            pending_copy(&index, &key, &before)?;
            must_record = declared;
        }
        if std::fs::symlink_metadata(&path).is_err() && record_count(&index)? >= MAX_ROOTS {
            return None;
        }
        let mut receipt = match std::fs::DirBuilder::new().mode(0o700).create(root) {
            Ok(()) => {
                must_record = true;
                let receipt = Receipt {
                    schema: 1,
                    root: root.to_path_buf(),
                    identity: identity(root)?,
                    nonce: aterm_uds::rand::hex_token::<16>().ok()?,
                    owners: Vec::new(),
                    owners_unknown: false,
                    pending: None,
                };
                write(&root.join(MARKER), &receipt)?;
                receipt
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let mut receipt = read(&path)?;
                if receipt.root != root || !same(home, &receipt) {
                    return None;
                }
                must_record = true;
                if receipt.pending.take().is_some() {
                    write(&path, &receipt)?;
                }
                receipt
            }
            Err(_) => return None,
        };
        if receipt.owners.iter().any(|owner| owner == workspace) {
            return Some(());
        } else {
            if receipt.owners.len() < MAX_OWNERS {
                receipt.owners.push(workspace.to_path_buf());
            } else {
                receipt.owners_unknown = true;
            }
        }
        write(&path, &receipt)
    };
    if attempt().is_some() {
        Ok(true)
    } else if must_record {
        Err(std::io::Error::other(
            "the managed build cache's owner could not be recorded",
        ))
    } else {
        Ok(false)
    }
}

#[cfg(not(unix))]
pub fn create_checked(_: &Path, _: &Path, _: &Path) -> std::io::Result<bool> {
    Ok(false)
}

/// The independently registered cache roots, without walking any workspace or home folder.
#[must_use]
pub fn roots(home: &Path) -> Vec<Registered> {
    let index = home.join(INDEX);
    if identity(&index).is_none() || std::fs::canonicalize(&index).ok().as_ref() != Some(&index) {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(&index) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "json")
        })
        .take(MAX_ROOTS)
        .filter_map(|entry| read(&entry.path()).map(|receipt| (entry.path(), receipt)))
        .filter(|(_, receipt)| same(home, receipt) || receipt.pending.is_some())
        .map(|(receipt_path, receipt)| Registered {
            root: receipt.root,
            owners: receipt.owners,
            owners_unknown: receipt.owners_unknown,
            receipt_path,
            identity: receipt.identity,
            nonce: receipt.nonce,
        })
        .collect()
}

#[cfg(unix)]
fn settle(path: &Path, receipt: &mut Receipt) {
    let still_root = identity(&receipt.root).as_ref() == Some(&receipt.identity);
    if !still_root
        || path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().ends_with(".retired.json"))
    {
        let _ = std::fs::remove_file(path);
    } else {
        receipt.pending = None;
        let _ = write(path, receipt);
    }
}

fn pending_owned(home: &Path, receipt: &Receipt, pending: &Pending) -> bool {
    let payload = pending.directory.join("cache.noindex");
    scope(home, &pending.directory)
        && std::fs::canonicalize(&pending.directory).ok().as_ref() == Some(&pending.directory)
        && identity(&pending.directory).as_ref() == Some(&pending.identity)
        && (pending.authorized
            || (identity(&payload).as_ref() == Some(&receipt.identity)
                && read(&payload.join(MARKER)).is_some_and(|marker| {
                    marker.identity == receipt.identity && marker.nonce == receipt.nonce
                })))
}

fn workspace(home: &Path, manifest: &Path) -> Option<PathBuf> {
    let manifest = std::fs::canonicalize(manifest).ok()?;
    if !super::checkout::MANIFESTS
        .iter()
        .any(|name| manifest.file_name() == Some(name.as_ref()))
    {
        return None;
    }
    let package = manifest.parent()?;
    if !scope(home, package) {
        return None;
    }
    let text = crate::metadata_io::read_bounded_regular_utf8(&manifest, 1 << 20).ok()?;
    let parsed = aterm_toml::from_str::<aterm_toml::Value>(&text).ok()?;
    if parsed.get("workspace").is_some() {
        return Some(package.to_path_buf());
    }
    if let Some(explicit) = parsed
        .get("package")
        .and_then(|table| table.get("workspace"))
        .and_then(aterm_toml::Value::as_str)
    {
        let owner = std::fs::canonicalize(package.join(explicit)).ok()?;
        if !scope(home, &owner) {
            return None;
        }
        let text =
            crate::metadata_io::read_bounded_regular_utf8(&owner.join("Cargo.toml"), 1 << 20)
                .ok()?;
        let parsed = aterm_toml::from_str::<aterm_toml::Value>(&text).ok()?;
        return parsed.get("workspace").is_some().then_some(owner);
    }
    for ancestor in package.ancestors().skip(1).take(32) {
        if !scope(home, ancestor) {
            break;
        }
        let path = ancestor.join("Cargo.toml");
        if let Ok(text) = crate::metadata_io::read_bounded_regular_utf8(&path, 1 << 20) {
            let parsed = aterm_toml::from_str::<aterm_toml::Value>(&text).ok()?;
            if parsed.get("workspace").is_some() {
                return Some(ancestor.to_path_buf());
            }
        }
    }
    Some(package.to_path_buf())
}

fn target_name(args: &[String], machine: &super::Machine<'_>) -> Option<PathBuf> {
    let mut target = None;
    let mut words = args.iter().map(String::as_str);
    while let Some(word) = words.next() {
        match word {
            "--" => break,
            "--target-dir" => target = words.next().map(PathBuf::from),
            "--manifest-path" | "--config" | "--color" | "--explain" | "-Z" | "-C" => {
                let _ = words.next();
            }
            _ => {
                if let Some(path) = word.strip_prefix("--target-dir=") {
                    target = Some(PathBuf::from(path));
                }
            }
        }
    }
    let target = target
        .or_else(|| (machine.env)("CARGO_TARGET_DIR").map(PathBuf::from))
        .or_else(|| (machine.env)("CARGO_BUILD_TARGET_DIR").map(PathBuf::from))?;
    if target.as_os_str().is_empty() {
        return None;
    }
    let absolute = if target.is_absolute() {
        target
    } else {
        machine.cwd?.join(target)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            component => normalized.push(component.as_os_str()),
        }
    }
    Some(normalized)
}

#[cfg(unix)]
fn suppress_unresolved(home: &Path, target: &Path) -> std::io::Result<bool> {
    let candidates = [target.to_path_buf(), target.join(super::TARGET_BUILD_LEAF)];
    let mut protected = false;
    for root in candidates {
        if !scope(home, &root)
            || std::fs::symlink_metadata(root.join(MARKER))
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        {
            continue;
        }
        let stop =
            || std::io::Error::other("the managed cache's unknown owner could not be recorded");
        let index = index(home).ok_or_else(stop)?;
        let key = key(&root).ok_or_else(stop)?;
        let path = index.join(format!("{key}.json"));
        let lock = crate::metadata_io::open_regular(&gate_path(&path).ok_or_else(stop)?)?;
        let _held = crate::lock::Flock::try_lock(lock).map_err(|_| stop())?;
        let mut receipt = read(&path)
            .filter(|receipt| receipt.root == root && same(home, receipt))
            .ok_or_else(stop)?;
        receipt.owners_unknown = true;
        write(&path, &receipt).ok_or_else(stop)?;
        protected = true;
    }
    Ok(protected)
}

#[cfg(not(unix))]
fn suppress_unresolved(_: &Path, _: &Path) -> std::io::Result<bool> {
    Ok(false)
}

/// An unresolvable directory config might select any currently registered root. Poison
/// whole-retirement authority conservatively, never claim its fresh target or change paths.
#[cfg(unix)]
fn suppress_configured(home: &Path) -> std::io::Result<()> {
    let directory = home.join(INDEX);
    if std::fs::symlink_metadata(&directory)
        .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    {
        return Ok(());
    }
    let stop = || std::io::Error::other("configured build cache owners could not be recorded");
    let directory = index(home).ok_or_else(stop)?;
    let admission =
        crate::metadata_io::open_regular(&directory.join("locks.noindex/admission.lock"))?;
    let _admission = crate::lock::Flock::lock(admission)?;
    for entry in std::fs::read_dir(&directory)? {
        let path = entry?.path();
        if !path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            continue;
        }
        let lock = crate::metadata_io::open_regular(&gate_path(&path).ok_or_else(stop)?)?;
        let _held = crate::lock::Flock::try_lock(lock).map_err(|_| stop())?;
        let mut receipt = read(&path).ok_or_else(stop)?;
        receipt.owners_unknown = true;
        write(&path, &receipt).ok_or_else(stop)?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn suppress_configured(_: &Path) -> std::io::Result<()> {
    Ok(())
}

fn observe_on(
    args: &[String],
    tool: super::Tool,
    home: &Path,
    machine: &super::Machine<'_>,
) -> std::io::Result<bool> {
    if args
        .iter()
        .take_while(|word| word.as_str() != "--")
        .any(|word| matches!(word.as_str(), "-h" | "--help" | "-V" | "--version"))
    {
        return Ok(false);
    }
    let Some(chosen) = super::explicit_cache(args, tool, machine) else {
        let protected = match target_name(args, machine) {
            Some(target) => suppress_unresolved(home, &target)?,
            None => false,
        };
        if super::unresolved_directory(args, machine) {
            suppress_configured(home)?;
        }
        return Ok(protected);
    };
    let Some(owner) = workspace(home, &chosen.manifest) else {
        return suppress_unresolved(home, &chosen.target);
    };
    let owned = create_checked(home, &chosen.target, &owner)?;
    if !owned && let Some(super::BuildDir::Target(placement)) = super::decide(args, tool, machine) {
        let child = chosen.target.join(super::TARGET_BUILD_LEAF);
        if placement == format!("build.build-dir='{}'", child.display()) {
            return create_checked(home, &child, &owner);
        }
    }
    Ok(owned)
}

/// Observe a real managed compiler invocation. This adds no flag and moves no output. The
/// last explicit target overrides the environment; an explicit manifest owns the cache even
/// when a launcher runs from another directory. Existing unknown caches remain unmanaged.
pub fn observe_command(
    args: &[String],
    tool: super::Tool,
    cwd: Option<&Path>,
) -> std::io::Result<()> {
    command_observation(args, tool, cwd).map(|_| ())
}

fn command_observation(
    args: &[String],
    tool: super::Tool,
    cwd: Option<&Path>,
) -> std::io::Result<bool> {
    if !cfg!(target_os = "macos") {
        return Ok(false);
    }
    let Some(home) = aterm_types::dirs::home_dir() else {
        return Ok(false);
    };
    let machine = super::Machine {
        home: Some(&home),
        root: None,
        cwd,
        env: &|name| std::env::var_os(name),
        read: &|path| match crate::metadata_io::read_bounded_regular_utf8(path, 1 << 20) {
            Ok(text) => Ok(Some(text)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(()),
        },
        writable: &|_| None,
    };
    observe_on(args, tool, &home, &machine)
}

/// The generated shim's optional Rust call, before its existing placement block. The helper
/// is stamped beside that shim at installation, never selected from the process environment.
#[must_use]
#[cfg(any(unix, test))]
pub fn shim_call(helper: &Path, tool: &str) -> String {
    if !matches!(tool, "targo" | "targo-tippy") {
        return String::new();
    }
    let quoted = super::super::platform::sh_quote_str(&helper.to_string_lossy());
    let marker = aterm_json::to_string(helper.to_string_lossy().as_ref()).unwrap_or_default();
    format!(
        "# atpkg cache helper {marker}\n\
         # aterm: register only newly created compiler caches at their existing output paths\n\
         if [ -x {quoted} ]; then\n  \
         {quoted} {REGISTER_VERB} {tool} \"$@\" >/dev/null 2>&1\n  \
         __atpkg_cache_status=$?\n  \
         if [ \"$__atpkg_cache_status\" -ne 0 ]; then\n    \
         printf '%s\\n' 'atpkg: a build cache record is busy or unavailable; no build started' >&2\n    \
         exit 75\n  \
         fi\n\
         else\n  \
         printf '%s\\n' 'atpkg: the cache registrar is unavailable; run aterm pkg repair; no build started' >&2\n  \
         exit 75\n\
         fi\n"
    )
}

/// The bounded backend hint a previous managed compiler shim recorded. Keep a standing hint
/// even if it has disappeared: silently dropping its guard would lose cache-owner observations.
#[must_use]
pub fn shim_helper(text: &str) -> Option<PathBuf> {
    let named = text
        .lines()
        .find_map(|line| line.strip_prefix("# atpkg cache helper "))?;
    let path = PathBuf::from(aterm_json::from_str::<String>(named).ok()?);
    path.is_absolute().then_some(path)
}

/// Internal compiler-shim entry, with no store/profile/credential discovery. A producer's
/// `--require-owned` refuses an unmanaged observation instead of acknowledging a marked cache.
pub fn run_hidden(args: &[std::ffi::OsString]) -> std::process::ExitCode {
    if args.len() == 1 && args[0] == "--capabilities" {
        println!("{REGISTER_CAPABILITY}");
        return std::process::ExitCode::SUCCESS;
    }
    let require_owned = args.first().is_some_and(|word| word == "--require-owned");
    let args = if require_owned { &args[1..] } else { args };
    let Some(tool) = args
        .first()
        .and_then(|word| word.to_str())
        .and_then(|word| match word {
            "targo" => Some(super::Tool::Targo),
            "targo-tippy" => Some(super::Tool::Tippy),
            _ => None,
        })
    else {
        return std::process::ExitCode::from(2);
    };
    let Some(words) = args[1..]
        .iter()
        .map(|word| word.to_str().map(str::to_owned))
        .collect::<Option<Vec<_>>>()
    else {
        return std::process::ExitCode::from(2);
    };
    match command_observation(&words, tool, std::env::current_dir().ok().as_deref()) {
        Ok(owned) if owned || !require_owned => std::process::ExitCode::SUCCESS,
        Ok(_) | Err(_) => std::process::ExitCode::from(75),
    }
}

fn owners_gone(home: &Path, receipt: &Receipt, placing: Option<&super::checkout::Placing>) -> bool {
    !receipt.owners_unknown
        && !receipt.owners.is_empty()
        && receipt.owners.iter().all(|owner| {
            scope(home, owner)
                && std::fs::symlink_metadata(owner)
                    .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
                && super::checkout::gone_placed(owner, Some(home), placing).is_ok()
        })
}

/// Recover a retirement that stopped before moving the original cache. Called under its
/// receipt gate: only the same still-current root and an unchanged owner receipt authorize
/// clearing an empty, identity-bound private stage, or its already-missing name.
#[cfg(unix)]
fn recover_unstarted(
    home: &Path,
    path: &Path,
    receipt: &mut Receipt,
    fence: &super::fence::Fence<'_>,
) -> bool {
    let Some(pending) = receipt.pending.clone() else {
        return false;
    };
    let current = |receipt: &Receipt| same(home, receipt) && read(path).as_ref() == Some(receipt);
    if !scope(home, &pending.directory) || !current(receipt) {
        return false;
    }
    match std::fs::symlink_metadata(&pending.directory) {
        Ok(_) => {
            if identity(&pending.directory).as_ref() != Some(&pending.identity)
                || pending.identity.device != receipt.identity.device
                || std::fs::canonicalize(&pending.directory).ok().as_ref()
                    != Some(&pending.directory)
            {
                return false;
            }
            let Ok(mut entries) = std::fs::read_dir(&pending.directory) else {
                return false;
            };
            if entries.next().is_some() {
                return false;
            }
            drop(entries);
            if !fence.clear(&pending.directory, &pending.directory)
                || !current(receipt)
                || identity(&pending.directory).as_ref() != Some(&pending.identity)
                || std::fs::remove_dir(&pending.directory).is_err()
            {
                return false;
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return false,
    }
    if !current(receipt) {
        return false;
    }
    let mut cleared = receipt.clone();
    cleared.pending = None;
    if write(path, &cleared).is_none() {
        return false;
    }
    *receipt = cleared;
    true
}

/// The eligible registered-cache parts, including held locks, for the existing read-only
/// machine-cache planner. These are advisory candidates: cleanup rechecks active use, which
/// this planner does not probe. Current-compiler dependencies stay while any owner still exists.
#[must_use]
pub fn plan(home: &Path, now: std::time::SystemTime) -> Vec<super::Planned> {
    let mut planned = Vec::new();
    for cache in roots(home) {
        let Some(lock_path) = gate_path(&cache.receipt_path) else {
            continue;
        };
        let Ok(lock) = crate::metadata_io::open_regular(&lock_path) else {
            continue;
        };
        let Ok(_held_record) = crate::lock::Flock::try_lock(lock) else {
            continue;
        };
        let Some(receipt) = read(&cache.receipt_path) else {
            continue;
        };
        if let Some(pending) = &receipt.pending {
            if pending_owned(home, &receipt, pending) {
                planned.push(super::Planned {
                    path: pending.directory.clone(),
                    paths: vec![pending.directory.clone()],
                    why: super::Unused::Unfinished,
                    held: false,
                });
            }
            continue;
        }
        if !same(home, &receipt) {
            continue;
        }
        let Some(seen) = super::look(&cache.root) else {
            continue;
        };
        if seen.locks.is_empty()
            || super::locks_under(&cache.root).ok().as_ref() != Some(&seen.locks)
        {
            continue;
        }
        let stage_start = planned.len();
        planned.extend(
            super::stages_in(&cache.root, std::ffi::OsStr::new(super::DOOMED))
                .into_iter()
                .map(|stage| super::Planned {
                    path: stage.clone(),
                    paths: vec![stage],
                    why: super::Unused::Unfinished,
                    held: false,
                }),
        );
        let held = super::hold(&seen.locks).is_none();
        let idle = super::age(now, seen.newest);
        if idle >= super::RECLAIM_GONE && owners_gone(home, &receipt, None) {
            // An unheld whole-root retirement already contains these stages.
            if !held {
                planned.truncate(stage_start);
            }
            planned.push(super::Planned {
                path: cache.root.clone(),
                paths: vec![cache.root],
                why: super::Unused::Gone {
                    idle,
                    after: super::RECLAIM_GONE,
                },
                held,
            });
            continue;
        }
        let store = super::compiler_store(Some(home));
        let doomed = super::stale::decide(
            &super::stale::survey(
                &super::stale::Disk,
                &cache.root,
                &super::profiles_of(&seen.locks),
                store.as_deref(),
            ),
            now,
        );
        if !doomed.is_empty() {
            let incremental = doomed
                .iter()
                .filter(|part| part.part == super::stale::Part::Incremental)
                .count();
            let units = doomed.len() - incremental;
            planned.push(super::Planned {
                path: cache.root,
                paths: doomed.into_iter().flat_map(|part| part.paths).collect(),
                why: super::Unused::Stale {
                    incremental,
                    units,
                    after: super::RECLAIM_STALE,
                },
                held,
            });
        }
    }
    planned
}

/// A registered root's partial cleanup uses exactly the normal cache's stale-unit rule.
/// Whole retirement additionally requires every recorded workspace to have disappeared.
/// The record lock, complete Cargo-lock scan and identity are checked again before acting.
#[cfg(unix)]
pub fn reclaim(
    home: &Path,
    now: std::time::SystemTime,
    gone_after: std::time::Duration,
    placing: Option<&super::checkout::Placing>,
    record: &mut dyn FnMut(&Path, super::Unused) -> bool,
) -> Vec<(PathBuf, super::Unused)> {
    reclaim_on(
        home,
        now,
        gone_after,
        placing,
        record,
        &super::fence::Fence::default(),
    )
}

#[cfg(unix)]
pub(super) fn reclaim_on(
    home: &Path,
    now: std::time::SystemTime,
    gone_after: std::time::Duration,
    placing: Option<&super::checkout::Placing>,
    record: &mut dyn FnMut(&Path, super::Unused) -> bool,
    fence: &super::fence::Fence<'_>,
) -> Vec<(PathBuf, super::Unused)> {
    let mut removed = Vec::new();
    for cache in roots(home) {
        let Some(lock_path) = gate_path(&cache.receipt_path) else {
            continue;
        };
        let Ok(lock) = crate::metadata_io::open_regular(&lock_path) else {
            continue;
        };
        let Ok(_held_record) = crate::lock::Flock::try_lock(lock) else {
            continue;
        };
        let Some(mut receipt) = read(&cache.receipt_path) else {
            continue;
        };
        if receipt.root != cache.root || !scope(home, &receipt.root) {
            continue;
        }
        if let Some(mut pending) = receipt.pending.clone() {
            if !pending_owned(home, &receipt, &pending) {
                let _ = recover_unstarted(home, &cache.receipt_path, &mut receipt, fence);
                continue;
            }
            if fence.clear(&cache.root, &pending.directory)
                && record(&pending.directory, super::Unused::Unfinished)
            {
                pending.authorized = true;
                receipt.pending = Some(pending.clone());
                if write(&cache.receipt_path, &receipt).is_none() {
                    continue;
                }
                if !fence.clear(&cache.root, &pending.directory) {
                    continue;
                }
                let left = super::remove::tree(&pending.directory);
                if left.whole() {
                    removed.push((pending.directory.clone(), super::Unused::Unfinished));
                    settle(&cache.receipt_path, &mut receipt);
                } else {
                    removed.push((
                        pending.directory.clone(),
                        super::Unused::Stayed {
                            stayed: left.stayed,
                            freed: left.freed,
                        },
                    ));
                }
            }
            continue;
        }
        if !same(home, &receipt) {
            continue;
        }
        let Some(seen) = super::look(&cache.root) else {
            continue;
        };
        // A missing or deeper lock is uncertainty, not an idle-cache witness.
        if seen.locks.is_empty()
            || super::locks_under(&cache.root).ok().as_ref() != Some(&seen.locks)
        {
            continue;
        }
        let gone = owners_gone(home, &receipt, placing);
        let whole = gone.then(|| {
            super::unused(&cache.root, &|seen| {
                let idle = super::age(now, seen.newest);
                (idle >= gone_after).then_some(super::Unused::Gone {
                    idle,
                    after: gone_after,
                })
            })
        });
        if let Some(Some((held_cargo, why))) = whole {
            if !same(home, &receipt)
                || !fence.clear(&cache.root, &cache.root)
                || !record(&cache.root, why)
                || !fence.clear(&cache.root, &cache.root)
            {
                continue;
            }
            let Some(parent) = cache.root.parent() else {
                continue;
            };
            let Ok(stage) = aterm_tempfile::Builder::new()
                .prefix("atpkg-cache-retiring-")
                .tempdir_in(parent)
            else {
                continue;
            };
            let directory = stage.keep();
            let Some(stage_id) = identity(&directory) else {
                continue;
            };
            receipt.pending = Some(Pending {
                directory: directory.clone(),
                identity: stage_id,
                authorized: false,
            });
            // Keep the private stage and its receipt on every failure, including a crash.
            if write(&cache.receipt_path, &receipt).is_none() {
                continue;
            }
            let staged = directory.join("cache.noindex");
            if !same(home, &receipt)
                || !owners_gone(home, &receipt, placing)
                || super::locks_under(&cache.root).ok().as_ref() != Some(&seen.locks)
                || !fence.clear(&cache.root, &cache.root)
                || std::fs::rename(&cache.root, &staged).is_err()
            {
                continue;
            }
            // A pathname could have been replaced between the last check and rename. Such
            // bytes are kept staged, never passed to the remover as this cache's payload.
            if !receipt
                .pending
                .as_ref()
                .is_some_and(|pending| pending_owned(home, &receipt, pending))
            {
                continue;
            }
            if !fence.clear(&cache.root, &directory) {
                continue;
            }
            if let Some(pending) = receipt.pending.as_mut() {
                pending.authorized = true;
            }
            if write(&cache.receipt_path, &receipt).is_none() {
                continue;
            }
            drop(held_cargo);
            if !fence.clear(&cache.root, &directory) {
                continue;
            }
            let left = super::remove::tree(&directory);
            if left.whole() {
                removed.push((cache.root, why));
                settle(&cache.receipt_path, &mut receipt);
            } else {
                removed.push((
                    directory,
                    super::Unused::Stayed {
                        stayed: left.stayed,
                        freed: left.freed,
                    },
                ));
            }
        } else {
            let mut sweep = super::Sweep {
                root: &cache.root,
                home: Some(home),
                now,
                gone_after,
                placing,
                moved: false,
                compiler_store: super::compiler_store(Some(home)),
                record,
                fence,
                removed: &mut removed,
            };
            // `prune` makes a fresh stage; only `walk` finishes older stages now. A registered
            // target is not a mirror tree, so finish just its own stages under this receipt gate.
            for stage in super::stages_in(&cache.root, std::ffi::OsStr::new(super::DOOMED)) {
                if !same(home, &receipt) {
                    break;
                }
                if sweep.stage_recorded(&stage, &cache.root) && same(home, &receipt) {
                    sweep.finish_stage(&stage, &cache.root);
                }
            }
            if same(home, &receipt) {
                let _ = sweep.prune(&cache.root, &cache.root);
            }
        }
    }
    removed
}

#[cfg(not(unix))]
pub fn reclaim(
    _: &Path,
    _: std::time::SystemTime,
    _: std::time::Duration,
    _: Option<&super::checkout::Placing>,
    _: &mut dyn FnMut(&Path, super::Unused) -> bool,
) -> Vec<(PathBuf, super::Unused)> {
    Vec::new()
}

/// An interrupted, identity-bound retirement fixture, after its image proved execution.
#[cfg(all(test, unix))]
pub(super) fn stage_for_process_test(home: &Path, root: &Path, stage: &Path) {
    let path = home
        .join(INDEX)
        .join(format!("{}.json", key(root).unwrap()));
    let mut receipt = read(&path).unwrap();
    std::fs::create_dir(stage).unwrap();
    receipt.pending = Some(Pending {
        directory: stage.to_path_buf(),
        identity: identity(stage).unwrap(),
        authorized: false,
    });
    write(&path, &receipt).unwrap();
    std::fs::rename(root, stage.join("cache.noindex")).unwrap();
    assert!(pending_owned(
        home,
        &receipt,
        receipt.pending.as_ref().unwrap()
    ));
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
    use std::time::{Duration, SystemTime};

    struct World {
        _directory: aterm_tempfile::TempDir,
        home: PathBuf,
        owner: PathBuf,
        target: PathBuf,
    }

    impl World {
        fn new() -> Self {
            let directory = aterm_tempfile::tempdir().unwrap();
            let home = directory.path().canonicalize().unwrap().join("home");
            std::fs::create_dir(&home).unwrap();
            let owner = home.join("source");
            std::fs::create_dir(&owner).unwrap();
            std::fs::write(owner.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
            Self {
                _directory: directory,
                target: home.join("outputs.noindex"),
                home,
                owner,
            }
        }

        fn another_owner(&self) -> PathBuf {
            let owner = self.home.join("another-source");
            std::fs::create_dir(&owner).unwrap();
            std::fs::write(owner.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
            owner
        }

        fn build(&self) {
            assert!(create_checked(&self.home, &self.target, &self.owner).unwrap());
            std::fs::create_dir_all(self.target.join("debug/deps")).unwrap();
            std::fs::write(self.target.join("debug/deps/keep"), b"reusable output").unwrap();
            std::fs::write(self.target.join("debug/.cargo-lock"), b"").unwrap();
        }

        fn path(&self) -> PathBuf {
            self.home
                .join(INDEX)
                .join(format!("{}.json", key(&self.target).unwrap()))
        }

        fn later(&self) -> SystemTime {
            SystemTime::now() + Duration::from_secs(2 * 24 * 60 * 60)
        }
    }

    #[test]
    fn an_unstarted_retirement_recovers_only_its_unchanged_empty_or_missing_stage() {
        #[derive(Clone, Copy, Debug)]
        enum Stage {
            Empty,
            Missing,
            Nonempty,
            HeldEmpty,
            OriginalInUse,
            Replaced,
            ChangedReceipt,
            ChangedRoot,
        }
        for state in [
            Stage::Empty,
            Stage::Missing,
            Stage::Nonempty,
            Stage::HeldEmpty,
            Stage::OriginalInUse,
            Stage::Replaced,
            Stage::ChangedReceipt,
            Stage::ChangedRoot,
        ] {
            let world = World::new();
            world.build();
            let path = world.path();
            let lock = crate::metadata_io::open_regular(&gate_path(&path).unwrap()).unwrap();
            let _held = crate::lock::Flock::try_lock(lock).unwrap();
            let mut receipt = read(&path).unwrap();
            let stage = aterm_tempfile::Builder::new()
                .prefix("atpkg-cache-retiring-")
                .tempdir_in(&world.home)
                .unwrap()
                .keep();
            receipt.pending = Some(Pending {
                directory: stage.clone(),
                identity: identity(&stage).unwrap(),
                authorized: false,
            });
            write(&path, &receipt).unwrap();
            match state {
                Stage::Empty | Stage::HeldEmpty | Stage::OriginalInUse => {}
                Stage::Missing => std::fs::remove_dir(&stage).unwrap(),
                Stage::Nonempty => std::fs::write(stage.join("keep"), b"owner data").unwrap(),
                Stage::Replaced => {
                    std::fs::rename(&stage, stage.with_extension("original")).unwrap();
                    std::fs::create_dir(&stage).unwrap();
                }
                Stage::ChangedReceipt => {
                    let mut changed = receipt.clone();
                    changed.owners_unknown = true;
                    write(&path, &changed).unwrap();
                }
                Stage::ChangedRoot => {
                    std::fs::rename(&world.target, world.home.join("original.noindex")).unwrap();
                    std::fs::create_dir(&world.target).unwrap();
                }
            }
            let before = std::fs::read(&path).unwrap();
            let recover = matches!(state, Stage::Empty | Stage::Missing | Stage::OriginalInUse);
            let processes = |dirs: &[PathBuf], _: &[u32]| {
                assert!(dirs.contains(&stage));
                let reported = match state {
                    Stage::HeldEmpty => Some(("cwd", stage.clone())),
                    Stage::OriginalInUse => Some(("exe", world.target.join("debug/deps/keep"))),
                    _ => None,
                };
                Ok(aterm_uds::holders::PathScan {
                    users: reported
                        .filter(|(_, path)| dirs.iter().any(|dir| path.starts_with(dir)))
                        .map(|(how, path)| aterm_uds::holders::PathUse { pid: 34, how, path })
                        .into_iter()
                        .collect(),
                    ..Default::default()
                })
            };
            assert_eq!(
                recover_unstarted(
                    &world.home,
                    &path,
                    &mut receipt,
                    &super::super::fence::Fence::new(&processes),
                ),
                recover,
                "{state:?}"
            );
            if recover {
                assert!(receipt.pending.is_none() && !stage.exists(), "{state:?}");
                assert!(roots(&world.home)[0].is_current(&world.home), "{state:?}");
            } else {
                assert_eq!(std::fs::read(&path).unwrap(), before, "{state:?}");
                assert!(receipt.pending.is_some() && stage.is_dir(), "{state:?}");
                if matches!(state, Stage::Nonempty) {
                    assert_eq!(std::fs::read(stage.join("keep")).unwrap(), b"owner data");
                }
            }
            let original = if matches!(state, Stage::ChangedRoot) {
                world.home.join("original.noindex")
            } else {
                world.target.clone()
            };
            assert_eq!(
                std::fs::read(original.join("debug/deps/keep")).unwrap(),
                b"reusable output"
            );
        }
    }

    #[test]
    fn fresh_creation_preserves_output_locations_and_never_adopts_an_existing_target() {
        let world = World::new();
        world.build();
        assert!(world.target.join("debug/deps/keep").is_file());
        assert!(!world.target.join("build.noindex").exists());
        let unknown = world.home.join("existing.noindex");
        std::fs::create_dir(&unknown).unwrap();
        std::fs::write(unknown.join("keep"), b"existing data").unwrap();
        assert!(!create_checked(&world.home, &unknown, &world.owner).unwrap());
        assert_eq!(
            std::fs::read(unknown.join("keep")).unwrap(),
            b"existing data"
        );
        assert!(!unknown.join(MARKER).exists());
        assert_eq!(roots(&world.home).len(), 1);
    }

    #[test]
    fn observations_distinguish_unmanaged_outputs_from_registered_roots() {
        let world = World::new();
        world.build();
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(&world.owner),
            env: &|_| None,
            read: &|_| Ok(None),
            writable: &|_| Some(true),
        };
        let args = |manifest: &Path, target: &Path| {
            vec![
                "--unverified".into(),
                "build".into(),
                format!("--manifest-path={}", manifest.display()),
                format!("--target-dir={}", target.display()),
            ]
        };
        let original = std::fs::read(world.path()).unwrap();
        let other = world.home.join("other-managed.noindex");
        assert!(create_checked(&world.home, &other, &world.owner).unwrap());
        let other_receipt = world
            .home
            .join(INDEX)
            .join(format!("{}.json", key(&other).unwrap()));
        std::fs::copy(other_receipt, world.path()).unwrap();
        assert!(
            observe_on(
                &[
                    "--unverified".into(),
                    "unknown-alias".into(),
                    format!("--target-dir={}", world.target.display())
                ],
                super::super::Tool::Targo,
                &world.home,
                &machine,
            )
            .is_err(),
            "another root's receipt cannot acknowledge the requested target"
        );
        std::fs::write(world.path(), original).unwrap();
        assert!(
            observe_on(
                &args(&world.owner.join("Cargo.toml"), &world.target),
                super::super::Tool::Targo,
                &world.home,
                &machine,
            )
            .unwrap()
        );
        let legacy = world.home.join("legacy.noindex");
        std::fs::create_dir(&legacy).unwrap();
        assert!(
            !observe_on(
                &args(&world.owner.join("Cargo.toml"), &legacy),
                super::super::Tool::Targo,
                &world.home,
                &machine,
            )
            .unwrap()
        );
        assert!(!legacy.join(MARKER).exists());

        // A target can be managed under one HOME, yet out of scope beneath a nested HOME's
        // protected roots. Exit zero for an unmanaged observation must not acknowledge it.
        let nested = world.home.join("nested-home");
        let protected = nested.join("Library/Containers/cache.noindex");
        std::fs::create_dir_all(protected.parent().unwrap()).unwrap();
        assert!(create_checked(&world.home, &protected, &world.owner).unwrap());
        let project = nested.join("project");
        std::fs::create_dir(&project).unwrap();
        std::fs::write(project.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
        let nested_machine = super::super::Machine {
            home: Some(&nested),
            cwd: Some(&project),
            ..machine
        };
        assert!(
            !observe_on(
                &args(&project.join("Cargo.toml"), &protected),
                super::super::Tool::Targo,
                &nested,
                &nested_machine,
            )
            .unwrap()
        );
        assert!(already_recorded(&world.home, &protected, &world.owner));
    }

    #[test]
    fn every_shared_owner_and_live_cargo_lock_retains_current_dependencies() {
        let world = World::new();
        world.build();
        let other = world.another_owner();
        assert!(create_checked(&world.home, &world.target, &other).unwrap());
        std::fs::remove_dir_all(&world.owner).unwrap();
        assert!(
            reclaim(
                &world.home,
                world.later(),
                super::super::RECLAIM_GONE,
                None,
                &mut |_, _| true
            )
            .is_empty()
        );
        assert!(world.target.join("debug/deps/keep").is_file());
        std::fs::remove_dir_all(&other).unwrap();
        let file = std::fs::File::open(world.target.join("debug/.cargo-lock")).unwrap();
        let held = crate::lock::Flock::try_lock(file).unwrap();
        assert!(
            reclaim(
                &world.home,
                world.later(),
                super::super::RECLAIM_GONE,
                None,
                &mut |_, _| true
            )
            .is_empty()
        );
        drop(held);
        let removed = reclaim(
            &world.home,
            world.later(),
            super::super::RECLAIM_GONE,
            None,
            &mut |_, _| true,
        );
        assert_eq!(removed.len(), 1);
        assert!(!world.target.exists());
    }

    #[test]
    fn an_interrupted_stale_stage_is_planned_and_retried_under_its_receipt_gate() {
        use std::os::unix::fs::MetadataExt as _;
        struct Clear;
        impl Drop for Clear {
            fn drop(&mut self) {
                super::super::remove::seams::clear();
            }
        }
        let world = World::new();
        world.build();
        let incremental = world.target.join("debug/incremental/old-unit");
        std::fs::create_dir_all(&incremental).unwrap();
        let held = incremental.join("held");
        std::fs::write(&held, b"unfinished output").unwrap();
        let _clear = Clear;
        super::super::remove::seams::other_user(std::fs::symlink_metadata(&held).unwrap().ino());
        let now = SystemTime::now() + super::super::RECLAIM_STALE + Duration::from_secs(60);
        let went = reclaim(
            &world.home,
            now,
            super::super::RECLAIM_GONE,
            None,
            &mut |_, _| true,
        );
        assert!(
            went.iter()
                .any(|(_, why)| matches!(why, super::super::Unused::Stayed { .. })),
            "{went:?}"
        );
        let stages =
            super::super::stages_in(&world.target, std::ffi::OsStr::new(super::super::DOOMED));
        assert_eq!(stages.len(), 1, "the real stale prune left one stage");
        let stage = &stages[0];
        assert!(!incremental.exists());
        assert!(world.target.join("debug/deps/keep").is_file());
        super::super::remove::seams::clear();
        assert!(
            plan(&world.home, now)
                .iter()
                .any(|p| p.path == *stage && p.why == super::super::Unused::Unfinished)
        );

        let file = crate::metadata_io::open_regular(&gate_path(&world.path()).unwrap()).unwrap();
        let gate = crate::lock::Flock::try_lock(file).unwrap();
        assert!(
            plan(&world.home, now).is_empty(),
            "a busy receipt gate admits no stage plan"
        );
        assert!(
            reclaim(
                &world.home,
                now,
                super::super::RECLAIM_GONE,
                None,
                &mut |_, _| true
            )
            .is_empty()
        );
        drop(gate);
        assert!(
            reclaim(
                &world.home,
                now,
                super::super::RECLAIM_GONE,
                None,
                &mut |p, _| p != stage
            )
            .is_empty()
        );
        assert!(stage.is_dir(), "a refused intent keeps the stage");
        let retried = reclaim(
            &world.home,
            now,
            super::super::RECLAIM_GONE,
            None,
            &mut |_, _| true,
        );
        assert_eq!(retried, [(stage.clone(), super::super::Unused::Unfinished)]);
        assert!(!stage.exists());
        assert!(
            world.target.join("debug/deps/keep").is_file(),
            "the live owner's current dependencies stay"
        );
        assert!(plan(&world.home, now).is_empty());
    }

    #[test]
    fn a_marked_cache_with_missing_or_corrupt_owner_records_refuses_another_producer() {
        let world = World::new();
        world.build();
        let other = world.another_owner();
        let before = std::fs::read(world.path()).unwrap();
        for record in [None, Some(b"{".as_slice())] {
            match record {
                None => std::fs::remove_file(world.path()).unwrap(),
                Some(bytes) => std::fs::write(world.path(), bytes).unwrap(),
            }
            assert!(create_checked(&world.home, &world.target, &other).is_err());
            assert!(world.target.join("debug/deps/keep").is_file());
            std::fs::write(world.path(), &before).unwrap();
        }
        let gate = crate::metadata_io::open_regular(&gate_path(&world.path()).unwrap()).unwrap();
        let _held = crate::lock::Flock::try_lock(gate).unwrap();
        assert!(create_checked(&world.home, &world.target, &other).is_err());
    }

    #[test]
    fn pending_stage_survives_fresh_replacement_and_finishes_without_touching_new_outputs() {
        let world = World::new();
        world.build();
        let mut before = read(&world.path()).unwrap();
        let stage = world.home.join("interrupted-retirement.noindex");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&stage)
            .unwrap();
        before.pending = Some(Pending {
            directory: stage.clone(),
            identity: identity(&stage).unwrap(),
            authorized: false,
        });
        write(&world.path(), &before).unwrap();
        std::fs::rename(&world.target, stage.join("cache.noindex")).unwrap();
        let owner = world.another_owner();
        assert!(create_checked(&world.home, &world.target, &owner).unwrap());
        std::fs::write(world.target.join("new-output"), b"keep the new generation").unwrap();
        assert_eq!(
            roots(&world.home).len(),
            2,
            "the old stage has independent custody"
        );
        let removed = reclaim(
            &world.home,
            world.later(),
            super::super::RECLAIM_GONE,
            None,
            &mut |_, _| true,
        );
        assert_eq!(removed.len(), 1);
        assert!(!stage.exists());
        assert_eq!(
            std::fs::read(world.target.join("new-output")).unwrap(),
            b"keep the new generation"
        );
        assert_eq!(roots(&world.home).len(), 1);
    }

    #[test]
    fn replacement_and_symlink_races_never_authorize_unrecorded_bytes() {
        let world = World::new();
        world.build();
        std::fs::remove_dir_all(&world.owner).unwrap();
        let old = world.home.join("old-output.noindex");
        let mut swapped = false;
        let removed = reclaim(
            &world.home,
            world.later(),
            super::super::RECLAIM_GONE,
            None,
            &mut |path, _| {
                if path == world.target && !swapped {
                    std::fs::rename(&world.target, &old).unwrap();
                    std::fs::create_dir(&world.target).unwrap();
                    std::fs::write(world.target.join("unowned"), b"preserve").unwrap();
                    swapped = true;
                }
                true
            },
        );
        assert!(swapped && removed.is_empty());
        assert_eq!(
            std::fs::read(world.target.join("unowned")).unwrap(),
            b"preserve"
        );
        assert!(old.join("debug/deps/keep").exists());
        let alias = world.home.join("alias.noindex");
        std::os::unix::fs::symlink(&world.target, &alias).unwrap();
        let owner = world.another_owner();
        assert!(!create_checked(&world.home, &alias, &owner).unwrap());
    }

    #[test]
    fn explicit_manifest_and_target_precedence_bind_the_real_workspace() {
        let world = World::new();
        let launcher = world.home.join("launcher");
        std::fs::create_dir(&launcher).unwrap();
        let target = world.home.join("chosen.noindex");
        let env_target = world.home.join("not-chosen.noindex");
        let env =
            |name: &str| (name == "CARGO_TARGET_DIR").then(|| env_target.as_os_str().to_owned());
        let read = |path: &Path| match std::fs::read_to_string(path) {
            Ok(text) => Ok(Some(text)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(()),
        };
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(&launcher),
            env: &env,
            read: &read,
            writable: &|_| Some(true),
        };
        let args = vec![
            "--unverified".into(),
            "test".into(),
            format!(
                "--manifest-path={}",
                world.owner.join("Cargo.toml").display()
            ),
            format!("--target-dir={}", target.display()),
        ];
        observe_on(&args, super::super::Tool::Targo, &world.home, &machine).unwrap();
        assert!(!env_target.exists());
        let recorded = roots(&world.home);
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].root, target);
        assert_eq!(recorded[0].owners, [world.owner]);
    }

    #[test]
    fn unknown_invocations_suppress_retirement_but_never_claim_fresh_targets() {
        let world = World::new();
        world.build();
        let env =
            |name: &str| (name == "CARGO_TARGET_DIR").then(|| world.target.as_os_str().to_owned());
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(&world.owner),
            env: &env,
            read: &|_| Ok(None),
            writable: &|_| Some(true),
        };
        let args = ["--unverified".into(), "an-unknown-alias".into()];
        observe_on(&args, super::super::Tool::Targo, &world.home, &machine).unwrap();
        assert!(read(&world.path()).unwrap().owners_unknown);
        let fresh = world.home.join("unknown-fresh.noindex");
        let env = |name: &str| (name == "CARGO_TARGET_DIR").then(|| fresh.as_os_str().to_owned());
        let machine = super::super::Machine {
            env: &env,
            ..machine
        };
        observe_on(&args, super::super::Tool::Targo, &world.home, &machine).unwrap();
        assert!(!fresh.exists());
    }

    #[test]
    fn configured_targets_preserve_an_unobserved_shared_owner() {
        let world = World::new();
        world.build();
        let owner = world.another_owner();
        let config = owner.join(".cargo/config.toml");
        let read_config = |path: &Path| {
            Ok((path == config)
                .then(|| format!("[build]\ntarget-dir = '{}'\n", world.target.display())))
        };
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(&owner),
            env: &|_| None,
            read: &read_config,
            writable: &|_| Some(true),
        };
        observe_on(
            &["--unverified".into(), "build".into()],
            super::super::Tool::Targo,
            &world.home,
            &machine,
        )
        .unwrap();
        std::fs::remove_dir_all(&world.owner).unwrap();
        assert!(read(&world.path()).unwrap().owners_unknown);
        assert!(
            reclaim(
                &world.home,
                world.later(),
                super::super::RECLAIM_GONE,
                None,
                &mut |_, _| true
            )
            .is_empty()
        );
        assert!(world.target.join("debug/deps/keep").exists());
        let args = [
            "--unverified".into(),
            "build".into(),
            "--config".into(),
            format!("build.target-dir='{}'", world.target.display()),
        ];
        observe_on(&args, super::super::Tool::Targo, &world.home, &machine).unwrap();
        assert!(!owner.join("target").exists());
        let mut receipt = read(&world.path()).unwrap();
        receipt.owners_unknown = false;
        write(&world.path(), &receipt).unwrap();
        let args = [
            "-C".into(),
            "another-launcher".into(),
            "test".into(),
            "--target-dir".into(),
            "../outputs.noindex".into(),
        ];
        observe_on(&args, super::super::Tool::Cargo, &world.home, &machine).unwrap();
        assert!(read(&world.path()).unwrap().owners_unknown);
    }

    #[test]
    fn interrupted_staging_never_authorizes_an_unrelated_payload() {
        let world = World::new();
        world.build();
        // Two interrupted attempts can belong to the same root generation. Each stage must
        // retain its own custody without making the next producer permanently unavailable.
        for name in ["interrupted-a.noindex", "interrupted-b.noindex"] {
            let mut receipt = read(&world.path()).unwrap();
            let stage = world.home.join(name);
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&stage)
                .unwrap();
            let payload = stage.join("cache.noindex");
            std::fs::create_dir(&payload).unwrap();
            std::fs::write(payload.join("unowned"), b"preserve").unwrap();
            receipt.pending = Some(Pending {
                directory: stage.clone(),
                identity: identity(&stage).unwrap(),
                authorized: false,
            });
            write(&world.path(), &receipt).unwrap();
            assert!(
                reclaim(
                    &world.home,
                    world.later(),
                    super::super::RECLAIM_GONE,
                    None,
                    &mut |_, _| true
                )
                .is_empty()
            );
            assert_eq!(std::fs::read(payload.join("unowned")).unwrap(), b"preserve");
            assert!(create_checked(&world.home, &world.target, &world.owner).unwrap());
            assert!(read(&world.path()).unwrap().pending.is_none());
            assert!(payload.exists());
        }
        assert_eq!(roots(&world.home).len(), 3);
        assert!(world.target.join("debug/deps/keep").exists());
    }

    #[test]
    fn helper_failure_or_interruption_starts_no_compiler() {
        let world = World::new();
        let helper = world.home.join("registrar");
        let shim = world.home.join("targo");
        let log = world.home.join("ran");
        for exit in [75, 130, 2] {
            std::fs::write(&helper, format!("#!/bin/sh\nexit {exit}\n")).unwrap();
            std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
            std::fs::write(
                &shim,
                format!(
                    "#!/bin/sh\n{}\nprintf 'compiler\\n' >> \"$ATPKG_TEST_CACHE_LOG\"\n",
                    shim_call(&helper, "targo")
                ),
            )
            .unwrap();
            let mut command = aterm_uds::spawnfd::command("/bin/sh");
            command
                .arg(&shim)
                .env_clear()
                .env("ATPKG_TEST_CACHE_LOG", &log);
            let result =
                aterm_uds::deadline::output_within(&mut command, Duration::from_secs(60)).unwrap();
            assert_eq!(result.status.code(), Some(75));
            assert!(!log.exists());
        }
        std::fs::write(&helper, "#!/bin/sh\nexit 0\n").unwrap();
        let mut command = aterm_uds::spawnfd::command("/bin/sh");
        command
            .arg(&shim)
            .env_clear()
            .env("ATPKG_TEST_CACHE_LOG", &log);
        let result =
            aterm_uds::deadline::output_within(&mut command, Duration::from_secs(60)).unwrap();
        assert!(result.status.success());
        assert_eq!(std::fs::read_to_string(log).unwrap(), "compiler\n");
    }

    #[test]
    fn a_contained_recorded_owner_needs_no_index_write_authority() {
        let world = World::new();
        world.build();
        let original = std::fs::read(world.path()).unwrap();
        let other_root = world.home.join("another-root.noindex");
        assert!(create_checked(&world.home, &other_root, &world.owner).unwrap());
        let other_path = world
            .home
            .join(INDEX)
            .join(format!("{}.json", key(&other_root).unwrap()));
        std::fs::copy(other_path, world.path()).unwrap();
        assert!(create_checked(&world.home, &world.target, &world.owner).is_err());
        std::fs::write(world.path(), original).unwrap();
        let directory = world.home.join(INDEX);
        let gates = directory.join("locks.noindex");
        let gate = gate_path(&world.path()).unwrap();
        for path in [&directory, &gates] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o500)).unwrap();
        }
        for path in [&world.path(), &gate] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o400)).unwrap();
        }
        assert!(create_checked(&world.home, &world.target, &world.owner).unwrap());
        let other = world.another_owner();
        assert!(create_checked(&world.home, &world.target, &other).is_err());
        for path in [&directory, &gates] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
}
