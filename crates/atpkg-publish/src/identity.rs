// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Machine signing identity, detached Ed25519 signature, and roster checks.

use std::path::{Path, PathBuf};

use crate::ctx::Ctx;
use crate::error::{Error, Result};

/// The secret machine key, loaded.
pub struct MachineKey {
    pkcs8: Vec<u8>,
    path: PathBuf,
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
        let pkcs8 = std::fs::read(path).map_err(|e| {
            Error::refused(format!(
                "no machine key at {} ({e}) — nothing here can sign a document any client \
                 accepts. Mint one from the paper master: `atpkg-keys setup --id <id>` (the \
                 first machine ever) or `atpkg-keys join --id <id>` (every later machine)",
                ctx.shown(path)
            ))
        })?;
        atpkg_keys::pubkey_b64(&pkcs8)
            .map_err(|e| Error::refused(format!("{}: {e}", ctx.shown(path))))?;
        Ok(Self {
            pkcs8,
            path: path.to_path_buf(),
        })
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
        let bytes =
            std::fs::read(doc).map_err(|e| Error::io(format!("read {}", doc.display()), &e))?;
        let sig = self.sign(&bytes)?;
        let mut out = doc.as_os_str().to_owned();
        out.push(".sig");
        let out = PathBuf::from(out);
        std::fs::write(&out, sig).map_err(|e| Error::io(format!("write {}", out.display()), &e))?;
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
    let text = std::fs::read_to_string(&ctx.identity.record).map_err(|e| {
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
mod tests {
    use super::*;
    use std::sync::Arc;

    /// `signer_for` reads `~/.aterm/release-ring/machine.toml` and ignores `~/.aterm/machine.toml`
    /// (`id != m22` in 0.146).
    /// NEGATIVE CONTROL: reading `~/.aterm/machine.toml` sees `"m99"` instead of `"m3"` and
    /// refuses it: its pubkey is not the key's.
    #[test]
    fn signer_for_reads_release_ring_and_ignores_host_machine_toml() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let home = dir.path();

        let ring = crate::testkit::Keys::mint("m3", &[]);
        let host = Arc::new(crate::host::fake::FakeHost::default());
        let ctx = ring.ctx(home, host);
        std::fs::write(
            home.join(".aterm/machine.toml"),
            "id = \"m99\"\npubkey = \"wrong-pubkey\"\n",
        )
        .expect("write host machine.toml");
        let signer = signer_for(&ctx, &ring.roster).expect("release-ring record signs");
        assert_eq!(signer.record.id, "m3");
    }
}
