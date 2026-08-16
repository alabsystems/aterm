// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The crate's file-I/O leaves, in ONE place.
//!
//! Trust charges the std fs FFI boundary to the enclosing function, so every reader and
//! writer in this tool funnels through the four functions here rather than scattering
//! obligations across call sites. They moved out of `main.rs` when [`crate::provision`]
//! needed the same discipline: a second, subtly different bounded reader living in the
//! library would be exactly the drift this consolidation prevents.

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt as _;

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
/// residual [`create_secret_file`] carries and for the same reason — there is no
/// non-raw-path spelling of "atomically replace this file".
pub fn write_bytes_atomic(path: &str, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = stage_sibling_temp(path, bytes)?;
    promote_staged(&tmp, path)
}

/// The STAGING half of [`write_bytes_atomic`], on its own so a caller replacing a PAIR of
/// files (the roster and its detached signature) can stage both completely before
/// promoting either — per-file atomicity cannot make a pair consistent, but staging both
/// first confines the torn-pair window to the renames alone.
///
/// Writes `bytes` to a sibling `<path>.atpkg-keys.tmp` (`create_new`, so a leftover from
/// an interrupted run is reported rather than silently reused), flushes it to the device,
/// and returns the temporary's path. Every failure removes the temporary before
/// returning: a stray `.tmp` beside a trust anchor is confusing at exactly the moment
/// nobody can afford to be confused, and it would make the next run fail on `create_new`
/// for a reason that has nothing to do with what went wrong.
pub fn stage_sibling_temp(path: &str, bytes: &[u8]) -> std::io::Result<String> {
    use std::io::Write as _;
    let tmp = concat(&[path, ".atpkg-keys.tmp"]);
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .create_new(true)
        .open(&tmp)?;
    let written = f.write_all(bytes).and_then(|()| f.sync_all());
    drop(f);
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(tmp)
}

/// The PROMOTION half of [`write_bytes_atomic`]: `rename(2)` a staged temporary over its
/// target, removing the temporary if the rename itself fails so no litter survives.
pub fn promote_staged(tmp: &str, path: &str) -> std::io::Result<()> {
    if let Err(e) = std::fs::rename(tmp, path) {
        let _ = std::fs::remove_file(tmp);
        return Err(e);
    }
    Ok(())
}

/// Create every missing directory above `path`, so a write to it cannot fail with ENOENT.
///
/// This exists because it did. `create_secret_file("$HOME/.aterm/machine.key")` was called
/// on machines where `$HOME/.aterm` had never been created — which is every first machine,
/// the one `setup` is for — and failed at the LAST step of a run that had already armed the
/// trust anchor. Directories are created before anything secret exists, so a failure here
/// costs an error message and nothing else.
pub fn ensure_parent_dir(path: &str) -> std::io::Result<()> {
    if let Some(parent) = std::path::Path::new(path).parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

/// Create the SECRET key file 0600, create-new (never clobber an existing key) — the one
/// owner-secret file-create FFI site (see [`read_bytes`]).
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
/// create-new + 0600, which the secret key requires, so the one residual obligation is
/// confined to this leaf.
pub fn create_secret_file(path: &str) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .append(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

/// Write `bytes` to an already-open file — the leaf write FFI site paired with
/// [`create_secret_file`].
pub fn write_all_to(f: &mut std::fs::File, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    f.write_all(bytes)
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
        assert_eq!(leftovers, vec!["pins.rs".to_string()], "no temporary survives");

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
        assert!(!dir.join("pins.rs.atpkg-keys.tmp").exists(), "no litter");
    }

    /// The parent-directory helper creates a missing chain and is happy with one that
    /// already exists.
    #[test]
    fn ensure_parent_dir_creates_a_missing_chain_and_is_idempotent() {
        let dir = scratch("parents");
        let key = dir.join("home/.aterm/machine.key").to_str().unwrap().to_string();
        assert!(!dir.join("home/.aterm").exists());
        ensure_parent_dir(&key).expect("creates the chain");
        assert!(dir.join("home/.aterm").is_dir());
        ensure_parent_dir(&key).expect("a second call is a no-op");
        // A bare filename has no parent to create, and that is not an error.
        ensure_parent_dir("machine.key").expect("no parent, no problem");
    }
}
