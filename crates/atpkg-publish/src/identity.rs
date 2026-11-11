// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Machine signing identity, detached Ed25519 signature, and roster checks.

use std::path::{Path, PathBuf};

use crate::ctx::Ctx;
use crate::error::{Error, Result};

/// Maximum size in bytes of a machine key file (`PKCS#8` Ed25519 is 83 bytes; allow generous
/// headroom while rejecting oversized files or streams).
const MAX_KEY_BYTES: u64 = 16 * 1024;

/// Maximum size in bytes of a metadata, manifest, index, or state file read by `atpkg-publish`.
pub(crate) const MAX_META_BYTES: u64 = 16 * 1024 * 1024;

/// Overwrite `buf` with zeroes through volatile writes so dead-store elimination keeps them.
fn scrub_bytes(buf: &mut [u8]) {
    for b in buf {
        // SAFETY: `b` is a live, uniquely borrowed, aligned `u8` from `buf`.
        unsafe { std::ptr::write_volatile(b, 0) };
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

pub(crate) fn open_regular_nonblock(
    path: &Path,
) -> std::io::Result<(std::fs::File, std::fs::Metadata)> {
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)?
    };
    #[cfg(not(unix))]
    let file = std::fs::File::open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    Ok((file, meta))
}

pub(crate) fn read_regular_bytes(path: &Path, max_bytes: u64) -> std::io::Result<Vec<u8>> {
    let (file, meta) = open_regular_nonblock(path)?;
    read_regular_file_bytes(file, &meta, max_bytes)
}

pub(crate) fn read_regular_file_bytes(
    file: std::fs::File,
    meta: &std::fs::Metadata,
    max_bytes: u64,
) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    if meta.len() > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("file exceeds {max_bytes} bytes"),
        ));
    }
    let cap = usize::try_from(meta.len().min(max_bytes).saturating_add(1)).unwrap_or(0);
    let mut buf = Vec::with_capacity(cap);
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut buf)?;
    if u64::try_from(buf.len()).unwrap_or(u64::MAX) > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("file exceeds {max_bytes} bytes"),
        ));
    }
    Ok(buf)
}

pub(crate) fn read_regular_text(path: &Path, max_bytes: u64) -> std::io::Result<String> {
    let bytes = read_regular_bytes(path, max_bytes)?;
    String::from_utf8(bytes).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

pub(crate) fn copy_regular_file(from: &Path, to: &Path) -> std::io::Result<std::fs::Metadata> {
    let (mut reader, meta) = open_regular_nonblock(from)?;
    let _ = std::fs::remove_file(to);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    let mut writer = opts.open(to)?;
    std::io::copy(&mut reader, &mut writer)?;
    writer.set_permissions(meta.permissions())?;
    Ok(meta)
}

#[cfg(unix)]
pub(crate) fn set_regular_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    file.set_permissions(std::fs::Permissions::from_mode(mode))
}

fn read_key_bytes(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    let (file, meta) = open_regular_nonblock(path)?;
    if meta.len() > MAX_KEY_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("key file exceeds {MAX_KEY_BYTES} bytes"),
        ));
    }
    let mut buf = Vec::with_capacity(usize::try_from(meta.len().saturating_add(1)).unwrap_or(0));
    if let Err(e) = file.take(MAX_KEY_BYTES + 1).read_to_end(&mut buf) {
        scrub_bytes(&mut buf);
        return Err(e);
    }
    if u64::try_from(buf.len()).unwrap_or(u64::MAX) > MAX_KEY_BYTES {
        scrub_bytes(&mut buf);
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("key file exceeds {MAX_KEY_BYTES} bytes"),
        ));
    }
    Ok(buf)
}

/// The secret machine key, loaded.
pub struct MachineKey {
    pkcs8: Vec<u8>,
    path: PathBuf,
}

impl Drop for MachineKey {
    fn drop(&mut self) {
        scrub_bytes(&mut self.pkcs8);
    }
}

impl std::fmt::Debug for MachineKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MachineKey")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl MachineKey {
    /// The machine key at `path`.
    ///
    /// # Errors
    ///
    /// A refusal when the key file cannot be read, naming the provisioning verbs that mint
    /// one, or when it is not a pkcs8 Ed25519 key.
    pub fn load(ctx: &Ctx, path: &Path) -> Result<Self> {
        let pkcs8 = read_key_bytes(path).map_err(|e| {
            Error::refused(format!(
                "no machine key at {} ({e}) — nothing here can sign a document any client \
                 accepts. Mint one from the paper master: `atpkg-keys setup --id <id>` (the \
                 first machine ever) or `atpkg-keys join --id <id>` (every later machine)",
                ctx.shown(path)
            ))
        })?;
        let key = Self {
            pkcs8,
            path: path.to_path_buf(),
        };
        atpkg_keys::pubkey_b64(&key.pkcs8)
            .map_err(|e| Error::refused(format!("{}: {e}", ctx.shown(path))))?;
        Ok(key)
    }

    /// Its base64 public key — what the roster lists for it.
    ///
    /// # Errors
    ///
    /// Never for a loaded key.
    pub fn pubkey_b64(&self) -> Result<String> {
        atpkg_keys::pubkey_b64(&self.pkcs8).map_err(Error::refused)
    }

    /// The detached signature over `bytes`, exactly.
    ///
    /// # Errors
    ///
    /// Never for a loaded key.
    pub fn sign(&self, bytes: &[u8]) -> Result<Vec<u8>> {
        atpkg_keys::sign(&self.pkcs8, bytes).map_err(Error::refused)
    }

    /// Sign `doc`'s bytes into `<doc>.sig` beside it.
    ///
    /// # Errors
    ///
    /// When `doc` cannot be read or the signature cannot be written.
    pub fn sign_file(&self, doc: &Path) -> Result<PathBuf> {
        let bytes = read_regular_bytes(doc, 64 * 1024 * 1024)
            .map_err(|e| Error::io(format!("read {}", doc.display()), &e))?;
        let sig = self.sign(&bytes)?;
        let mut out = doc.as_os_str().to_owned();
        out.push(".sig");
        let out = PathBuf::from(out);
        crate::lane::report::write_atomic_bytes(&out, &sig)?;
        Ok(out)
    }
}

/// The machine's public record (`~/.aterm/release-ring/machine.toml`): its roster id and pubkey.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MachineRecord {
    /// The roster id.
    pub id: String,
    /// The base64 public key.
    pub pubkey: String,
}

/// A loaded identity verified to sign an index under a given roster.
#[derive(Debug)]
pub struct Signer {
    /// The key.
    pub key: MachineKey,
    /// Its record.
    pub record: MachineRecord,
    /// The `roster_seq` of the roster it was checked against.
    pub roster_seq: u64,
}

/// Load the identity and check it against the roster it signs under.
///
/// # Errors
///
/// A refusal for a missing or malformed key or record, a key the record does not name, a
/// revoked id, a key the roster does not list, a roster that lists it under another id, or
/// an entry past its horizon.
pub fn signer_for(ctx: &Ctx, roster: &aterm_update_core::Roster) -> Result<Signer> {
    let key = MachineKey::load(ctx, &ctx.identity.key)?;
    let key_pub = key.pubkey_b64()?;
    let record = read_record(ctx, &key_pub)?;
    if record.pubkey != key_pub {
        return Err(Error::refused(format!(
            "{} is NOT the key {} names — the record says {}, the key file is {}. Two \
             provisioning runs have been crossed; sort out which machine this is before \
             signing anything as it",
            ctx.shown(&ctx.identity.key),
            ctx.shown(&ctx.identity.record),
            record.pubkey,
            key_pub
        )));
    }
    if roster.is_revoked(&record.id) {
        return Err(Error::refused(format!(
            "machine '{id}' is REVOKED on this roster (roster_seq {seq}). Revoked machines \
             leave the candidate set before any crypto runs, so no client ever checks this \
             machine's signature. A revoked id NEVER returns — `atpkg-keys join --id {id}` \
             is refused by design. Publish from a machine that is still listed, or provision \
             this box afresh under a NEW id: `atpkg-keys join --id <new-machine-id> --key \
             <new path>`, then point the identity at that key",
            id = record.id,
            seq = roster.roster_seq
        )));
    }
    let Some(listed) = roster.machines.iter().find(|m| m.pubkey == key_pub) else {
        return Err(Error::refused(format!(
            "machine '{id}' is NOT on the roster this publish would ship (roster_seq {seq}): \
             its key is not listed. If that roster is older than this machine, install the \
             current pair (from a machine that has it) at {roster}. Otherwise mint a \
             rostered key (needs the paper master): `atpkg-keys join --id {id} --key <new \
             path>`, then point the identity at that key",
            id = record.id,
            seq = roster.roster_seq,
            roster = ctx.shown(&ctx.identity.roster)
        )));
    };
    if listed.id != record.id {
        return Err(Error::refused(format!(
            "the roster lists this machine's key under id '{}', but {} calls it '{}'. The id \
             travels INSIDE the signed bytes and the client binds it to the key that \
             verified, so this index would be refused as a relabelling. Fix the local record \
             to match the roster; the roster is the authority",
            listed.id,
            ctx.shown(&ctx.identity.record),
            record.id
        )));
    }
    if let Err(why) = roster.machine(&record.id, ctx.now_unix) {
        return Err(Error::refused(format!(
            "machine '{id}' is listed on roster_seq {seq}, but the roster refuses it now \
             ({why:?}): every client would refuse what it signs. Publish from a live machine, \
             or have the paper master renew its entry",
            id = record.id,
            seq = roster.roster_seq
        )));
    }
    Ok(Signer {
        key,
        record,
        roster_seq: roster.roster_seq,
    })
}

/// The machine record, or the refusal that restores it by hand: `join` cannot (it refuses
/// while the key exists, and would mint a new one).
fn read_record(ctx: &Ctx, key_pub: &str) -> Result<MachineRecord> {
    let restore = || {
        format!(
            "restore it by hand, with the id the roster lists for this key:\n  id = \"<roster \
             id>\"\n  pubkey = \"{key_pub}\""
        )
    };
    let text = read_regular_bytes(&ctx.identity.record, 64 * 1024)
        .and_then(|bytes| {
            String::from_utf8(bytes)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
        })
        .map_err(|e| {
            Error::refused(format!(
                "no machine record at {} ({e}) — {}",
                ctx.shown(&ctx.identity.record),
                restore()
            ))
        })?;
    let table: aterm_toml::Table = text.parse().map_err(|e| {
        Error::refused(format!(
            "{} does not parse ({e}) — {}",
            ctx.shown(&ctx.identity.record),
            restore()
        ))
    })?;
    let field = |k: &str| {
        table
            .get(k)
            .and_then(aterm_toml::Value::as_str)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    match (field("id"), field("pubkey")) {
        (Some(id), Some(pubkey)) => Ok(MachineRecord { id, pubkey }),
        _ => Err(Error::refused(format!(
            "{} has no id/pubkey pair — {}",
            ctx.shown(&ctx.identity.record),
            restore()
        ))),
    }
}

#[cfg(test)]
#[path = "identity_tests.rs"]
mod tests;
