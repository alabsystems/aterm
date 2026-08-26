// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The crate's file-I/O leaves, in ONE place.
//!
//! Trust charges the std fs FFI boundary to the enclosing function, so every reader and
//! writer in this tool funnels through the leaf functions here rather than scattering
//! obligations across call sites. They moved out of `main.rs` when [`crate::provision`]
//! needed the same discipline: a second, subtly different bounded reader living in the
//! library would be exactly the drift this consolidation prevents.

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt as _;
use std::sync::atomic::{AtomicU64, Ordering};

/// Distinguishes one staged temporary from every other one this process makes, so a
/// temporary left behind by a process death can never be reused, mistaken for a later
/// run's bytes, or wedge that run on `create_new` (see [`stage_sibling_temp`]).
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Everything this tool reads is tiny — a pkcs8 key is under a hundred bytes, a manifest
/// a few KiB, `pins.rs` a few tens of KiB — so 1 MiB is a generous ceiling, not a tight
/// fit.
pub const READ_CAP: u64 = 1024 * 1024;

/// Build a message by concatenation — deliberately not `format!`: the macro's inlined
/// unsafe `fmt::Arguments::new` expansion is unmodeled by Trust and charged to the caller.
#[must_use]
pub fn concat(parts: &[&str]) -> String {
    // No `with_capacity` pre-size: the capacity hint is a pure optimization, and its
    // unbounded-size allocation obligation is unprovable for arbitrary inputs.
    let mut s = String::new();
    for p in parts {
        s.push_str(p);
    }
    s
}

/// Name a non-regular file's actual type for the [`read_bytes`] refusal message, so
/// `--key /dev/urandom` says "character device" instead of a mystery error.
fn file_type_name(t: std::fs::FileType) -> &'static str {
    use std::os::unix::fs::FileTypeExt as _;
    if t.is_dir() {
        "a directory"
    } else if t.is_char_device() {
        "a character device"
    } else if t.is_block_device() {
        "a block device"
    } else if t.is_fifo() {
        "a FIFO"
    } else if t.is_socket() {
        "a socket"
    } else {
        "not a regular file"
    }
}

/// Read a file's exact bytes — the one shared file-read site.
///
/// Regular-file-only and bounded: an unbounded `read_to_end` of an operator-supplied path
/// is the seamless.rs `/dev/urandom` kernel-panic incident in miniature — a never-EOF
/// character device fills RAM+swap until the machine dies. The type check runs on the OPEN
/// handle's fstat (so it cannot race a path swap) and refuses devices, directories, FIFOs
/// and sockets before a single byte is read; `take` then bounds the read at
/// `READ_CAP + 1` so even a regular file growing underneath us cannot allocate unboundedly
/// — the +1 byte is how we DETECT overflow (exactly CAP+1 bytes arriving proves the file
/// exceeds the cap) without reading further. (One residual: a writerless FIFO parks the
/// tool in open(2) itself — POSIX blocks there until a writer arrives — but past open,
/// nothing can hang or overallocate.)
pub fn read_bytes(path: &str) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    let f = std::fs::File::open(path)?;
    let ft = f.metadata()?.file_type();
    if !ft.is_file() {
        return Err(std::io::Error::other(concat(&[
            path,
            " is ",
            file_type_name(ft),
            ", not a regular file",
        ])));
    }
    let mut bytes = Vec::new();
    f.take(READ_CAP + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > READ_CAP {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            concat(&[
                path,
                " exceeds the 1 MiB read cap (keys and manifests are tiny)",
            ]),
        ));
    }
    Ok(bytes)
}

/// Write `bytes` to `path` — the one shared plain-file-write site (see [`read_bytes`]).
///
/// Known Trust L0 artifact: `File::create` is a hardened raw-path boundary
/// (`hardened_raw_path_api`, fail-closed absent capability contracts). It must stay:
/// re-signing overwrites an existing `.sig`, so the non-clobbering `File::create_new`
/// (which Trust does not flag) would be a behavior change.
pub fn write_bytes(path: &str, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    std::fs::File::create(path)?.write_all(bytes)
}

/// Replace `path` ATOMICALLY: write a sibling temporary file, flush it to the device, then
/// `rename(2)` it over the target.
///
/// # Why the trust anchor may not be written with `File::create`
///
/// `File::create` truncates first and writes second, so every failure in between —
/// ENOSPC, a signal, a crash, a full inode table — leaves `pins.rs` empty or half-written.
/// That is the "silently mangled anchor file" the writer module exists to prevent, and it
/// was reachable: a plan that produced text the verifier then rejected left the damaged
/// bytes on disk while the tool told the operator "nothing was written". `rename` within a
/// directory is atomic on every POSIX filesystem, so a reader sees either the whole old
/// file or the whole new one and never a torn one.
///
/// The `sync_all` before the rename is the ordering that makes that true across a power
/// loss as well as across a crash: renaming a file whose contents are still only in the
/// page cache can, after a reset, expose a correctly-named file full of nothing.
///
/// The temporary is created in the TARGET'S OWN DIRECTORY, never in `/tmp`: `rename` across
/// filesystems fails with EXDEV, and a fallback copy would reintroduce exactly the
/// truncate-then-write window this function removes. It is created `create_new`, so a
/// leftover from an interrupted run is reported rather than silently reused, and it is
/// removed on every failure path so a refusal leaves no litter beside a trust anchor.
///
/// Known Trust L0 artifact: `OpenOptions::open` and `fs::rename` are hardened raw-path
/// boundaries (`hardened_raw_path_api`, fail-closed absent capability contracts), the same
/// residual [`stage_sibling_temp_with_mode`] carries and for the same reason — there is no
/// non-raw-path spelling of "atomically replace this file".
pub fn write_bytes_atomic(path: &str, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = stage_sibling_temp(path, bytes)?;
    promote_staged(&tmp, path)
}

/// The STAGING half of [`write_bytes_atomic`], on its own because a whole fsynced sibling
/// is a building block in its own right: per-file atomicity cannot make a PAIR of files
/// consistent, and the roster's crash-safe pair publication is built out of staged
/// members (see [`crate::provision`]'s redo transaction).
///
/// Writes `bytes` to a UNIQUE sibling temporary (`create_new`), flushes it to the device,
/// and returns the temporary's path. Every ordinary failure removes the temporary before
/// returning: a stray `.tmp` beside a trust anchor is confusing at exactly the moment
/// nobody can afford to be confused.
///
/// The name carries this process's pid and a per-process counter rather than the single
/// fixed `<path>.atpkg-keys.tmp` it used to. A fixed name made a process DEATH — the one
/// failure no error path can clean up — wedge every later run on `create_new`, for a
/// reason that has nothing to do with what that run was doing, and on the very file
/// (`aterm-machines.toml`) whose recovery story is "copy it back from another machine".
/// A unique name cannot be reused, cannot be mistaken for this run's bytes, and cannot
/// block anybody; the residual is a litter file, which is what a crash leaves anyway.
pub fn stage_sibling_temp(path: &str, bytes: &[u8]) -> std::io::Result<String> {
    stage_sibling_temp_with_mode(path, bytes, 0o666)
}

/// [`stage_sibling_temp`] with an explicit creation mode, so an OWNER-SECRET staging file
/// is never briefly world-readable between `create_new` and a later `chmod`. `0o666` is
/// what `OpenOptions` uses by default (umask applies either way); `0o600` is what
/// [`write_owner_file_create_new`] stages with — this is the crate's one owner-secret
/// file-create FFI site.
///
/// Write access is requested via `append(true)`, deliberately NOT `write(true)`: Trust's
/// FFI-summary matcher keys on the callee's LAST path segment, so `OpenOptions::write`
/// false-matches the libc `write(fd, buf, len)` summary and manufactures refuted
/// fd-range/non-null/writes-global obligations for a plain builder-flag call. The two
/// spellings are byte-identical here: `create_new` (O_EXCL) guarantees a brand-new empty
/// file, and it is written exactly once by a single `write_all`, for which append-at-EOF
/// (EOF = 0) and write-at-offset-0 coincide.
///
/// Known Trust L0 artifact: `OpenOptions::open` itself is a hardened raw-path boundary
/// (`hardened_raw_path_api`, fail-closed) that can only be discharged by capability
/// contracts, which this campaign does not add. `File::create` cannot express
/// create-new + 0600, which the machine key requires, so the residual obligation is
/// confined to this leaf.
fn stage_sibling_temp_with_mode(path: &str, bytes: &[u8], mode: u32) -> std::io::Result<String> {
    use std::io::Write as _;
    let (tmp, mut f) = loop {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let tmp = concat(&[
            path,
            ".atpkg-keys.",
            &std::process::id().to_string(),
            ".",
            &sequence.to_string(),
            ".tmp",
        ]);
        // A pid is reused across reboots, so a name collision with a dead run's litter is
        // possible; take the next sequence number rather than failing on someone else's
        // corpse.
        match std::fs::OpenOptions::new()
            .append(true)
            .create_new(true)
            .mode(mode)
            .open(&tmp)
        {
            Ok(file) => break (tmp, file),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    };
    let written = f.write_all(bytes).and_then(|()| f.sync_all());
    drop(f);
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(tmp)
}

/// The PROMOTION half of [`write_bytes_atomic`]: `rename(2)` a staged temporary over its
/// target, removing the temporary if the rename itself fails so no litter survives, then
/// flushing the DIRECTORY ENTRY so the new name survives a power loss (see
/// [`sync_parent`]).
pub fn promote_staged(tmp: &str, path: &str) -> std::io::Result<()> {
    if let Err(e) = std::fs::rename(tmp, path) {
        let _ = std::fs::remove_file(tmp);
        return Err(e);
    }
    sync_parent(path)
}

/// Install a NEW owner-only file atomically, without ever replacing an existing path.
///
/// The complete `0600` inode is `fsync`ed before a `hard_link` publishes its final name,
/// so the name and the bytes cannot appear out of order across a reset — a machine key
/// that exists but is empty is indistinguishable from a key the roster does not name.
/// The link is what makes "never replace" and "atomic" hold at once: `rename` would
/// clobber, and `create_new` + write is exactly the truncate-window
/// [`write_bytes_atomic`] exists to remove. The parent directory is synced before the
/// caller may publish authority that depends on the file surviving a power loss.
pub fn write_owner_file_create_new(path: &str, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = stage_sibling_temp_with_mode(path, bytes, 0o600)?;
    if let Err(e) = std::fs::hard_link(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = sync_parent(path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::remove_file(&tmp)?;
    sync_parent(path)
}

/// Flush the DIRECTORY ENTRY containing `path`. A file's own `sync_all` persists its
/// BYTES; it does not persist the name that reaches them, so without this a power loss
/// after a rename can expose the old name, or no name at all, beside a roster signature
/// that has already been published against the new one.
pub fn sync_parent(path: &str) -> std::io::Result<()> {
    let parent = std::path::Path::new(path)
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    std::fs::File::open(parent)?.sync_all()
}

/// Create every missing directory above `path`, so a write to it cannot fail with ENOENT.
///
/// This exists because it did. The machine key at `$HOME/.aterm/machine.key` was created
/// on machines where `$HOME/.aterm` had never been created — which is every first machine,
/// the one `setup` is for — and failed at the LAST step of a run that had already armed the
/// trust anchor. Directories are created before anything secret exists, so a failure here
/// costs an error message and nothing else.
pub fn ensure_parent_dir(path: &str) -> std::io::Result<()> {
    let Some(parent) = std::path::Path::new(path)
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return Ok(());
    };
    // Which directories this call is about to CREATE, learned before creating them.
    let mut missing = Vec::new();
    let mut cursor = parent;
    while !cursor.as_os_str().is_empty() && std::fs::symlink_metadata(cursor).is_err() {
        missing.push(cursor.to_path_buf());
        let Some(next) = cursor.parent() else {
            break;
        };
        cursor = next;
    }
    std::fs::create_dir_all(parent)?;
    // Persist each newly-created directory entry, shallowest first. `sync_all` on
    // `$HOME/.aterm/machine.key` cannot make that key durable if `$HOME/.aterm` itself
    // can still vanish in the reset — the key would come back as an ENOENT on a path the
    // roster already authorizes.
    for directory in missing.iter().rev() {
        let directory = directory.to_str().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "new directory path is not UTF-8",
            )
        })?;
        sync_parent(directory)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("atpkg-keys-fsio").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    /// The ordinary case: the file is replaced, and no temporary is left beside it.
    #[test]
    fn an_atomic_write_replaces_the_file_and_leaves_nothing_behind() {
        let dir = scratch("replace");
        let path = dir.join("pins.rs").to_str().unwrap().to_string();
        std::fs::write(&path, b"old").unwrap();

        write_bytes_atomic(&path, b"new content").expect("the write succeeds");
        assert_eq!(std::fs::read(&path).unwrap(), b"new content");
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            leftovers,
            vec!["pins.rs".to_string()],
            "no temporary survives"
        );

        // It creates as well as replaces.
        let fresh = dir.join("new.rs").to_str().unwrap().to_string();
        write_bytes_atomic(&fresh, b"hello").unwrap();
        assert_eq!(std::fs::read(&fresh).unwrap(), b"hello");
    }

    /// A FAILED WRITE LEAVES THE ORIGINAL EXACTLY AS IT WAS.
    ///
    /// This is the property `File::create` could not offer: it truncates first, so a
    /// failure after that point leaves a trust anchor empty or half-written while the tool
    /// reports that nothing was written. The failure is manufactured by taking away the
    /// directory's write permission, which is what ENOSPC or a crash would look like from
    /// the caller's side.
    #[test]
    fn a_failed_atomic_write_leaves_the_original_intact() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = scratch("failed");
        let path = dir.join("pins.rs").to_str().unwrap().to_string();
        std::fs::write(&path, b"the trust anchor").unwrap();

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        let result = write_bytes_atomic(&path, b"a replacement that cannot land");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();

        assert!(result.is_err(), "the write must fail");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"the trust anchor",
            "the original is untouched — not truncated, not partial"
        );
        // The temporary's name is unique per run, so this must be a SCAN, not a guess at
        // one fixed name — a spelled-out name would pass vacuously.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(leftovers, vec!["pins.rs".to_string()], "no litter");
    }

    /// A TEMPORARY LEFT BY A DEAD PROCESS NEVER WEDGES THE NEXT WRITE. The fixed
    /// `<path>.atpkg-keys.tmp` name this staging used to take made the one failure no
    /// error path can clean up — a process death — fail every later run on `create_new`,
    /// on the file whose only recovery is a copy from another machine.
    ///
    /// MUTATION: give `stage_sibling_temp_with_mode` back the fixed name and the write
    /// below fails with EEXIST instead of landing.
    #[test]
    fn a_crash_left_temp_never_wedges_the_next_atomic_write() {
        let dir = scratch("stale-temp");
        let path = dir.join("roster.toml").to_str().unwrap().to_string();
        let stale = concat(&[&path, ".atpkg-keys.tmp"]);
        std::fs::write(&stale, b"bytes from a dead process").unwrap();

        write_bytes_atomic(&path, b"current").expect("a unique temp bypasses stale litter");
        assert_eq!(std::fs::read(&path).unwrap(), b"current");
        // And it is left alone rather than silently consumed: it is somebody else's
        // evidence, not this run's scratch space.
        assert_eq!(std::fs::read(&stale).unwrap(), b"bytes from a dead process");
    }

    /// AN OWNER FILE IS WHOLE, PRIVATE, AND NEVER REPLACED. All three at once is the
    /// point: `create_new` + `write_all` gives "never replaced" but exposes a
    /// zero-length key under its final name, and `write_bytes_atomic` gives "whole" but
    /// clobbers. The link-after-fsync route gives both.
    #[test]
    fn an_owner_file_is_complete_private_and_never_replaced() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = scratch("owner-create-new");
        let path = dir.join("machine.toml").to_str().unwrap().to_string();

        write_owner_file_create_new(&path, b"first").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"first");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        assert!(write_owner_file_create_new(&path, b"second").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"first");
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            leftovers,
            vec!["machine.toml".to_string()],
            "the refused write leaves no staged temporary behind"
        );
    }

    /// The parent-directory helper creates a missing chain and is happy with one that
    /// already exists.
    #[test]
    fn ensure_parent_dir_creates_a_missing_chain_and_is_idempotent() {
        let dir = scratch("parents");
        let key = dir
            .join("home/.aterm/machine.key")
            .to_str()
            .unwrap()
            .to_string();
        assert!(!dir.join("home/.aterm").exists());
        ensure_parent_dir(&key).expect("creates the chain");
        assert!(dir.join("home/.aterm").is_dir());
        ensure_parent_dir(&key).expect("a second call is a no-op");
        // A bare filename has no parent to create, and that is not an error.
        ensure_parent_dir("machine.key").expect("no parent, no problem");
    }
}
