// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Account-ledger adoption. A selector never authorizes releasing old debt.
//! The legacy parent loses creation rights first. Readiness waits for a later
//! authenticated kernel boot, proving old executions ended even when an old
//! final-close reaper erased their records. CPU backing stays in its child.

use crate::cohort_namespace::Namespace;
use aterm_dirfd::{self as fd, Mode, OFlags, RenameFlags};
use aterm_toolchain::record_lock::RecordLock;
use std::{
    fs::File,
    io::{self, Read as _, Write as _},
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
};

fn refused(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why)
}

fn paths(legacy: &Path) -> io::Result<(PathBuf, PathBuf)> {
    let name = legacy
        .file_name()
        .and_then(|v| v.to_str())
        .ok_or_else(|| refused("invalid account ledger name"))?;
    let parent = legacy
        .parent()
        .ok_or_else(|| refused("account ledger has no parent"))?;
    Ok((
        parent.join(format!(".{name}.memory-v3")),
        parent.join(format!(".{name}.memory-v3.pending")),
    ))
}
fn directory(path: &Path) -> io::Result<File> {
    let file = crate::cohort_namespace::directory(path)?;
    private(&file, true)?;
    Ok(file)
}

fn private(file: &File, directory: bool) -> io::Result<(u64, u64)> {
    let meta = file.metadata()?;
    // SAFETY: geteuid reads the current effective identity and has no arguments.
    let uid = unsafe { libc::geteuid() };
    if meta.uid() != uid
        || meta.mode() & 0o077 != 0
        || (directory && !meta.is_dir())
        || (!directory && (!meta.is_file() || meta.nlink() != 1))
    {
        return Err(refused("account namespace entry is not private and owned"));
    }
    Ok((meta.dev(), meta.ino()))
}
/// The selector's parent, a directory of this user's that no one else may write. One of this
/// user's that its group or others may write is narrowed first, through the descriptor checked
/// (as `aterm_uds::private_file::ensure_private_dir` narrows its own): Linux's user-private-group
/// umask, 002, leaves aterm's cache folder 0775 wherever a creator leaves the mode to the
/// umask. Another user's is refused (another user may write it), and so is one the narrowing
/// leaves writable.
fn outer(path: &Path) -> io::Result<File> {
    let file = crate::cohort_namespace::directory(path)?;
    let meta = file.metadata()?;
    // SAFETY: geteuid takes no arguments and reads only this process identity.
    if meta.uid() != unsafe { libc::geteuid() } {
        return Err(refused("account selector parent is writable by others"));
    }
    if meta.mode() & 0o022 != 0 {
        file.set_permissions(std::fs::Permissions::from_mode(
            meta.mode() & 0o7777 & !0o022,
        ))?;
        if file.metadata()?.mode() & 0o022 != 0 {
            return Err(refused("account selector parent is writable by others"));
        }
    }
    Ok(file)
}
fn record(path: &Path) -> io::Result<String> {
    let parent = outer(
        path.parent()
            .ok_or_else(|| refused("missing selector parent"))?,
    )?;
    let file = File::from(fd::openat(
        &parent,
        path.file_name()
            .ok_or_else(|| refused("missing selector name"))?,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    private(&file, false)?;
    let mut text = String::new();
    file.take(8193).read_to_string(&mut text)?;
    if text.len() > 8192 {
        return Err(refused("oversized account namespace record"));
    }
    Ok(text)
}
fn flush_record(path: &Path) -> io::Result<()> {
    let parent = outer(
        path.parent()
            .ok_or_else(|| refused("missing selector parent"))?,
    )?;
    let file = File::from(fd::openat(
        &parent,
        path.file_name()
            .ok_or_else(|| refused("missing selector name"))?,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    private(&file, false)?;
    aterm_durable::durable(&file)?;
    aterm_durable::durable(&parent)
}
fn decode(text: &str) -> io::Result<((u64, u64), Namespace, String, bool)> {
    let value: aterm_json::Value =
        aterm_json::from_str(text).map_err(|_| refused("invalid account namespace record"))?;
    let object = value
        .as_object()
        .ok_or_else(|| refused("invalid account namespace record"))?;
    if object.len() != 6 || value["version"].as_u64() != Some(3) {
        return Err(refused("unknown account namespace record"));
    }
    let dev = value["dev"]
        .as_u64()
        .ok_or_else(|| refused("missing account ledger device"))?;
    let ino = value["ino"]
        .as_u64()
        .ok_or_else(|| refused("missing account ledger inode"))?;
    let fence = value["fence"]
        .as_str()
        .ok_or_else(|| refused("missing retained namespace"))?;
    let namespace = Namespace::resolve(Path::new(fence))?
        .ok_or_else(|| refused("retained namespace is absent"))?;
    let boot = value["retired_boot"]
        .as_str()
        .filter(|boot| !boot.is_empty() && boot.len() <= 128)
        .ok_or_else(|| refused("missing retired boot identity"))?
        .to_owned();
    let fenced = value["fenced"]
        .as_bool()
        .ok_or_else(|| refused("missing barrier phase"))?;
    Ok(((dev, ino), namespace, boot, fenced))
}
fn denial(parent: &File) -> io::Result<()> {
    if parent.metadata()?.mode() & 0o222 != 0 {
        return Err(refused("legacy memory namespace remains writable"));
    }
    let name = format!(".adoption-denial-{}", aterm_uds::rand::hex_token::<16>()?);
    match fd::openat(
        parent,
        name.as_str(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    ) {
        Err(error) if matches!(error.raw_os_error(), libc::EACCES | libc::EPERM) => Ok(()),
        Err(error) => Err(io::Error::from(error)),
        Ok(file) => {
            drop(file);
            let _ = fd::unlinkat(parent, name.as_str(), fd::AtFlags::empty());
            Err(refused("ACL permits legacy memory claim creation"))
        }
    }
}

/// Resolve the mandatory selector for an account ledger. Prepared, deleted or
/// replaced activated state is a refusal, never a return to ordinary claims.
/// This is observation, not host leadership or settlement authority.
pub fn resolve(legacy: &Path) -> io::Result<Option<Namespace>> {
    let (ready, pending) = paths(legacy)?;
    if std::fs::symlink_metadata(&pending).is_ok() {
        return Err(refused("account memory adoption is unfinished"));
    }
    let text = match record(&ready) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => match directory(legacy) {
            Ok(parent) if parent.metadata()?.mode() & 0o222 == 0 => {
                return Err(refused("retired account ledger lost its selector"));
            }
            Ok(_) => return Ok(None),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        },
        Err(error) => return Err(error),
    };
    let (identity, namespace, retired_boot, fenced) = decode(&text)?;
    if !fenced {
        return Err(refused("account memory barrier is incomplete"));
    }
    let boot =
        aterm_uds::process::boot_id().ok_or_else(|| refused("current boot identity is unknown"))?;
    if retired_boot == boot {
        return Err(refused("legacy execution closure requires a later boot"));
    }
    let parent = directory(legacy)?;
    if private(&parent, true)? != identity {
        return Err(refused("retired account ledger was replaced"));
    }
    denial(&parent)?;
    namespace.validate()?;
    Ok(Some(namespace))
}

/// Adopt a freshly prepared retained namespace for ordinary account admission.
/// Nothing calls this implicitly. Any failure after intent publication leaves
/// the account unready, with the intent available for an exact retry.
///
/// # Errors
/// Root, live/unknown predecessor guards, ACL write access, changed identities,
/// partial/foreign intent, or a durability failure. Same-boot retirement returns
/// WouldBlock until a later authenticated boot. Never restores permissions.
pub fn adopt(legacy: &Path, namespace: &Namespace) -> io::Result<()> {
    let boot =
        aterm_uds::process::boot_id().ok_or_else(|| refused("current boot identity is unknown"))?;
    adopt_in_boot(legacy, namespace, &boot)
}

fn adopt_in_boot(legacy: &Path, namespace: &Namespace, boot: &str) -> io::Result<()> {
    // SAFETY: geteuid reads only the current process's effective identity.
    if unsafe { libc::geteuid() } == 0 {
        return Err(refused("root cannot prove account permission exclusion"));
    }
    namespace.validate()?;
    let (ready, pending) = paths(legacy)?;
    if ready.exists() && pending.exists() {
        return Err(refused("conflicting account adoption records"));
    }
    if let Some(actual) = resolve(legacy).or_else(|error| {
        if pending.exists() {
            Ok(None)
        } else {
            Err(error)
        }
    })? {
        if &actual != namespace {
            return Err(refused("account already adopted another namespace"));
        }
        // A previous rename may have succeeded before its directory flush
        // failed. Idempotent success must retry both data and publication.
        flush_record(&ready)?;
        if resolve(legacy)?.as_ref() != Some(namespace) {
            return Err(refused("account adoption changed during retry"));
        }
        return Ok(());
    }
    let parent = directory(legacy)?;
    let (dev, ino) = private(&parent, true)?;
    let (mut retired_boot, fenced) = prepare_intent(legacy, namespace, boot, (dev, ino))?;
    if fenced {
        // Never repair a revoked barrier and then credit its earlier boot.
        denial(&parent)?;
        aterm_durable::durable(&parent)?;
    } else {
        parent.set_permissions(std::fs::Permissions::from_mode(0o500))?;
        aterm_durable::durable(&parent)?;
        denial(&parent)?;
        publish_barrier(&pending, boot)?;
        retired_boot = boot.to_owned();
    }
    if retired_boot == boot {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "legacy memory creation retired; readiness requires a later authenticated boot",
        ));
    }
    let _guards = drain(legacy, &parent)?;
    if private(&directory(legacy)?, true)? != (dev, ino) {
        return Err(refused("account ledger changed during adoption"));
    }
    let outer = outer(
        legacy
            .parent()
            .ok_or_else(|| refused("missing ledger parent"))?,
    )?;
    fd::renameat_with(
        &outer,
        pending
            .file_name()
            .ok_or_else(|| refused("missing pending name"))?,
        &outer,
        ready
            .file_name()
            .ok_or_else(|| refused("missing ready name"))?,
        RenameFlags::NOREPLACE,
    )?;
    aterm_durable::durable(&outer)?;
    resolve(legacy)?.ok_or_else(|| refused("account adoption failed to publish"))?;
    Ok(())
}

fn prepare_intent(
    legacy: &Path,
    namespace: &Namespace,
    boot: &str,
    identity: (u64, u64),
) -> io::Result<(String, bool)> {
    let (_, pending) = paths(legacy)?;
    let (dev, ino) = identity;
    let fence = namespace.fence();
    let fence = fence
        .to_str()
        .ok_or_else(|| refused("retained namespace path is not UTF-8"))?;
    let mut retired_boot = boot.to_owned();
    let mut fenced = false;
    let expected =
        aterm_json::json!({"version":3,"dev":dev,"ino":ino,"fence":fence,"retired_boot":boot,"fenced":false})
            .to_string();
    match record(&pending) {
        Ok(text) => {
            let (identity, actual, original_boot, original_fenced) = decode(&text)?;
            if identity != (dev, ino) || &actual != namespace {
                return Err(refused("another account adoption intent exists"));
            }
            retired_boot = original_boot;
            fenced = original_fenced;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let selector_parent = outer(
                legacy
                    .parent()
                    .ok_or_else(|| refused("missing ledger parent"))?,
            )?;
            let mut file = File::from(fd::openat(
                &selector_parent,
                pending
                    .file_name()
                    .ok_or_else(|| refused("missing intent name"))?,
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::RUSR | Mode::WUSR,
            )?);
            file.write_all(expected.as_bytes())?;
            aterm_durable::durable(&file)?;
            aterm_durable::durable(&outer(
                legacy
                    .parent()
                    .ok_or_else(|| refused("missing ledger parent"))?,
            )?)?;
        }
        Err(error) => return Err(error),
    }
    // Exact intent recovery still must retry a previous failed data/directory
    // flush before relying on this record to retire the old namespace.
    flush_record(&pending)?;
    Ok((retired_boot, fenced))
}

fn publish_barrier(pending: &Path, boot: &str) -> io::Result<()> {
    let mut value: aterm_json::Value =
        aterm_json::from_str(&record(pending)?).map_err(|_| refused("invalid barrier intent"))?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| refused("invalid barrier intent"))?;
    object.insert(
        "retired_boot".to_owned(),
        aterm_json::Value::String(boot.to_owned()),
    );
    object.insert("fenced".to_owned(), aterm_json::Value::Bool(true));
    let outer = outer(
        pending
            .parent()
            .ok_or_else(|| refused("missing barrier parent"))?,
    )?;
    let stage = format!(".memory-barrier-{}", aterm_uds::rand::hex_token::<16>()?);
    let mut file = File::from(fd::openat(
        &outer,
        stage.as_str(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )?);
    file.write_all(value.to_string().as_bytes())?;
    aterm_durable::durable(&file)?;
    fd::renameat_with(
        &outer,
        stage.as_str(),
        &outer,
        pending
            .file_name()
            .ok_or_else(|| refused("missing barrier name"))?,
        RenameFlags::empty(),
    )?;
    aterm_durable::durable(&outer)
}

fn drain(path: &Path, parent: &File) -> io::Result<Vec<RecordLock>> {
    let mut guards = Vec::new();
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == crate::build_cpu::DIRECTORY_NAME {
            continue;
        }
        let name = name
            .to_str()
            .ok_or_else(|| refused("unknown legacy memory entry"))?;
        // The changed boot proves every pre-retirement execution ended. Keep
        // all original idle guard descriptions through commit; unknown names
        // or new live descriptions still refuse readiness.
        if name != "ledger.lock"
            && ![".taking", ".wait", ".claim"]
                .iter()
                .any(|suffix| name.ends_with(suffix))
        {
            return Err(refused("unknown legacy memory predecessor"));
        }
        let file = File::from(fd::openat(
            parent,
            name,
            OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )?);
        private(&file, false)?;
        match RecordLock::try_lock(file, crate::ledger::RECORD_MAX) {
            Ok(guard) => guards.push(guard),
            Err(_) => return Err(refused("legacy memory predecessor remains held or unknown")),
        }
    }
    Ok(guards)
}

#[cfg(test)]
#[path = "user_namespace_tests.rs"]
mod tests;
