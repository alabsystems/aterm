// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The producer half of the machine-roster tier: this machine's public identity, the cut-time gate that proves it is still authorized, and the stamp that puts attribution inside the signed appcast bytes.

use std::path::{Path, PathBuf};

use aterm_update_core::Manifest;
use aterm_update_core::roster::{Attribution, Roster, verify_roster};

use crate::ledger::{Error, Result};

#[cfg(unix)]
#[path = "machines/current_roster.rs"]
pub(crate) mod current_roster;

/// This machine's public identity, as `atpkg-keys setup`/`join` recorded it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct MachineIdentity {
    /// The machine id — `"m3"`.
    pub id: String,
    /// The machine's base64 Ed25519 public key.
    pub pubkey: String,
    /// RFC3339 mint time.
    #[serde(default)]
    pub minted_at: String,
}

impl MachineIdentity {
    /// Read the identity from `path`.
    pub fn read(path: &Path) -> Result<Option<Self>> {
        let text = match crate::durable::read_regular_text(path, 64 * 1024) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(Error::new(format!("read {}: {e}", path.display()))),
        };
        let identity: Self = aterm_toml::from_str(&text)
            .map_err(|e| Error::new(format!("parse {}: {e}", path.display())))?;
        if identity.id.is_empty() || identity.pubkey.is_empty() {
            return Err(Error::new(format!(
                "{} names no machine id or no public key; re-mint with `atpkg-keys \
                 join --id <id>`",
                path.display()
            )));
        }
        Ok(Some(identity))
    }
}

/// `home/.aterm/release-ring/machine.toml`, beside the key `atpkg-keys setup`/`join` wrote.
#[must_use]
pub fn identity_path_in(home: &Path) -> PathBuf {
    home.join(atpkg_keys::provision::MACHINE_PUB_REL)
}

/// `~/.aterm/release-ring/machine.toml`, beside the key `atpkg-keys setup`/`join` wrote.
pub fn conventional_identity_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| identity_path_in(Path::new(&home)))
}

/// What this machine claims its id is: the release-credentials profile's `machine_id` when set (it is part of the command that ran), else `~/.aterm/release-ring/machine.toml`.
pub fn declared_machine_id(
    profile_machine_id: Option<&str>,
    identity_path: Option<&Path>,
) -> Result<Option<String>> {
    if let Some(id) = profile_machine_id {
        return Ok(Some(id.to_string()));
    }
    let Some(path) = identity_path else {
        return Ok(None);
    };
    Ok(MachineIdentity::read(path)?.map(|identity| identity.id))
}

/// The master-signed roster and its detached signature, read once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RosterDocument {
    /// The exact `aterm-machines.toml` bytes, unparsed and unmodified.
    pub bytes: Vec<u8>,
    /// The master's detached Ed25519 signature over [`Self::bytes`].
    pub signature: Vec<u8>,
}

impl RosterDocument {
    /// The detached signature's conventional name, `<roster>.sig`: one name in the profile, so a second key cannot point at a signature over a different roster.
    #[must_use]
    pub fn signature_path(roster: &Path) -> PathBuf {
        let mut name = roster.as_os_str().to_os_string();
        name.push(".sig");
        PathBuf::from(name)
    }

    /// Read both halves, naming whichever is missing.
    pub fn read(roster: &Path) -> Result<Self> {
        let signature_path = Self::signature_path(roster);
        let bytes = crate::durable::read_regular_bytes(roster, 65_536).map_err(|e| {
            Error::new(format!(
                "read the machine roster {}: {e} — `machine_roster` in the \
                 release-credentials profile names it, so the cut cannot proceed without it",
                roster.display()
            ))
        })?;
        let signature =
            crate::durable::read_regular_bytes(&signature_path, 4_096).map_err(|e| {
                Error::new(format!(
                    "read the machine roster's master signature {}: {e} — it must sit beside \
                 the roster (`atpkg-keys setup`/`join` writes both)",
                    signature_path.display()
                ))
            })?;
        Ok(Self { bytes, signature })
    }
}

/// How much of the freshness window a cut must still have ahead of it.
pub const MIN_REMAINING_WINDOW_SECS: i64 = 6 * 60 * 60;

/// How long the cut's channel reads go on without progress: longer than a client's
/// ([`aterm_update_core::release_asset::PATIENCE`]), since a cut refused over a passing outage of
/// the download host is a whole cut run again, while a client simply checks later.
pub(crate) const CUT_READ_PATIENCE: std::time::Duration = std::time::Duration::from_secs(300);

/// The master-admitted roster on the public channel's latest release: its `roster_seq` and verified bytes, or `None` when that release carries no roster.
pub(crate) fn channel_roster_document(
    slug: &str,
) -> std::result::Result<Option<(u64, Vec<u8>)>, String> {
    let reader = crate::github::reader(CUT_READ_PATIENCE).map_err(|e| e.to_string())?;
    let Some(RosterDocument { bytes, signature }) = channel_roster_pair(&reader, slug)? else {
        return Ok(None);
    };
    let verified = verify_roster(
        aterm_update_core::pins::PAPER_MASTER_PUBKEYS,
        bytes,
        &signature,
    )
    .map_err(|e| {
        format!("the channel roster did not verify under the committed paper master ({e:?})")
    })?;
    let parsed = Roster::parse(&verified)
        .map_err(|e| format!("the channel roster verified but did not parse ({e:?})"))?;
    Ok(Some((parsed.roster_seq, verified.as_slice().to_vec())))
}

/// The roster and its signature on `slug`'s latest release, both read at the one tag the latest
/// names (so a release published between the two reads cannot pair one release's roster with
/// another's signature), or `None` when the channel has no release or that release no roster.
/// Through the shared reader ([`aterm_update_core::release_asset`]): the web route, then the API
/// route when the download host fails; a 404 is the asset's absence and asks nothing more. The
/// bytes are judged by the caller (the roster by its signature), so the route changes nothing
/// about what is accepted.
pub(crate) fn channel_roster_pair(
    reader: &aterm_update_core::release_asset::Reader,
    slug: &str,
) -> std::result::Result<Option<RosterDocument>, String> {
    use aterm_update_core::release_asset::{Asset, AssetError, Release};
    use aterm_update_core::roster::{ROSTER_ASSET, ROSTER_SIG_ASSET};
    let (owner, repo) = slug
        .split_once('/')
        .ok_or_else(|| format!("the channel {slug:?} is not owner/repo"))?;
    let tag = match reader.latest_tag(owner, repo, ROSTER_ASSET) {
        Ok(tag) => tag,
        Err(AssetError::Absent(_)) => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let at = |name: &'static str| {
        Asset::in_slug(slug, Release::Tag(&tag), name)
            .ok_or_else(|| format!("{slug} {tag} {name} is not a safe release asset address"))
    };
    let bytes = match reader.bytes(&at(ROSTER_ASSET)?, 65_536) {
        Ok(bytes) => bytes,
        Err(AssetError::Absent(_)) => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let signature = reader
        .bytes(&at(ROSTER_SIG_ASSET)?, 4_096)
        .map_err(|e| format!("the roster is present but its signature could not be read: {e}"))?;
    Ok(Some(RosterDocument { bytes, signature }))
}

/// At equal generation the cut's roster (the pair in `dist/`) and the channel's must be the same document; otherwise two lineages exist at one number, and whichever ships later strands every client that ratcheted on the other.
pub(crate) fn roster_lineage_agrees(
    local_roster: &[u8],
    carried_seq: Option<u64>,
    channel: Option<&(u64, Vec<u8>)>,
) -> std::result::Result<(), String> {
    match (carried_seq, channel) {
        (Some(carried), Some((observed, bytes)))
            if carried == *observed && local_roster != bytes.as_slice() =>
        {
            Err(format!(
                "LINEAGE FORK: this cut carries machine-roster generation {carried} and the public \
                 channel's head carries generation {observed} too, but the two documents differ \
                 — two machines minted the same generation from the same seed. Publishing \
                 either over the other strands every client that ratcheted on the first. Stop; \
                 re-join from the machine holding the channel's document (never start a second \
                 roster)"
            ))
        }
        _ => Ok(()),
    }
}

/// The cut-time gate: prove that `signing_pubkey` belongs to a machine the roster authorizes, and return the attribution the manifest will carry.
pub fn authorize_cut(
    master_pubkeys: &[&str],
    roster_bytes: Vec<u8>,
    roster_sig: &[u8],
    signing_pubkey: &str,
    now_unix: i64,
) -> Result<Attribution> {
    let verified = verify_roster(master_pubkeys, roster_bytes, roster_sig).map_err(|e| {
        Error::new(format!(
            "the machine roster does not verify under the pinned paper master ({e:?}); \
             refusing a cut no client would accept"
        ))
    })?;
    let roster = Roster::parse(&verified)
        .map_err(|e| Error::new(format!("the machine roster is unusable ({e:?})")))?;
    roster
        .admit(0, now_unix.saturating_add(MIN_REMAINING_WINDOW_SECS))
        .map_err(|e| {
            Error::new(format!(
                "the machine roster is not usable for a cut right now ({e:?}) — it must \
                 still be valid {MIN_REMAINING_WINDOW_SECS} seconds from now, because \
                 every client checks its window at a later clock than this one. Re-sign \
                 it with the paper master (`atpkg-keys join`/`machine-revoke` \
                 refresh the window)"
            ))
        })?;

    let me = roster
        .machines
        .iter()
        .find(|m| m.pubkey == signing_pubkey)
        .ok_or_else(|| {
            Error::new(format!(
                "the configured signing key's public identity {signing_pubkey} is not on \
                 the machine roster. Mint this machine a key with `atpkg-keys \
                 join --id <id>`, or cut from a machine that is listed"
            ))
        })?;
    let machine = roster.machine(&me.id, now_unix).map_err(|e| {
        Error::new(format!(
            "this machine ({}) may not sign: {e:?}. If it was revoked, that is the \
             roster working as intended",
            me.id
        ))
    })?;
    Ok(Attribution {
        machine_id: machine.id.clone(),
        pubkey_b64: machine.pubkey.clone(),
        roster_seq: roster.roster_seq,
    })
}

/// Which key the roster maps `id` to, for refusal messages only: the mismatch refusal in [`crate::publish::channel_signature_policy`] offers `cut with the key that belongs to <id>` only when the roster names that machine.
#[must_use]
pub fn roster_pubkey_for(
    master_pubkeys: &[&str],
    roster_bytes: Vec<u8>,
    roster_sig: &[u8],
    id: &str,
) -> Option<String> {
    let verified = verify_roster(master_pubkeys, roster_bytes, roster_sig).ok()?;
    let roster = Roster::parse(&verified).ok()?;
    roster
        .machines
        .iter()
        .find(|m| m.id == id)
        .map(|m| m.pubkey.clone())
}

/// Prove that a roster downloaded from an already published release (or about to be uploaded from `dist/`) is the document that release's manifest is attributed under, and return the asset's `roster_seq`.
pub fn verify_published_roster(
    master_pubkeys: &[&str],
    roster_bytes: Vec<u8>,
    roster_sig: &[u8],
    machine_id: &str,
    roster_seq: Option<u64>,
) -> Result<u64> {
    Ok(verify_published_roster_parsed(
        master_pubkeys,
        roster_bytes,
        roster_sig,
        machine_id,
        roster_seq,
    )?
    .roster_seq)
}

pub(crate) fn verify_published_roster_parsed(
    master_pubkeys: &[&str],
    roster_bytes: Vec<u8>,
    roster_sig: &[u8],
    machine_id: &str,
    roster_seq: Option<u64>,
) -> Result<Roster> {
    let verified = verify_roster(master_pubkeys, roster_bytes, roster_sig).map_err(|e| {
        Error::new(format!(
            "the published release's machine roster does not verify under this tree's \
             pinned paper master ({e:?}). A release that carries an attribution can only \
             be recovered by a tree that can check it — arm PAPER_MASTER_PUBKEYS with the \
             master that signed it, or finish this release by hand"
        ))
    })?;
    let roster = Roster::parse(&verified)
        .map_err(|e| Error::new(format!("the published machine roster is unusable ({e:?})")))?;
    match roster_seq {
        Some(claimed) if claimed <= roster.roster_seq => {}
        _ => {
            return Err(Error::new(format!(
                "the published release's manifest claims roster_seq {roster_seq:?} but the \
                 roster asset carries {}; refusing to reconstruct a roster the signed \
                 manifest does not name (a newer asset over an older attribution is \
                 admissible; this is not that)",
                roster.roster_seq
            )));
        }
    }
    if !roster.machines.iter().any(|m| m.id == machine_id) {
        return Err(Error::new(format!(
            "the published release is attributed to machine {machine_id:?}, which the \
             roster asset beside it does not list; refusing to reconstruct a roster that \
             contradicts the signed manifest"
        )));
    }
    Ok(roster)
}

/// Stamp attribution into the manifest before it is serialized and signed: `machine_id` makes the signature unrelabellable and `roster_seq` stops an old roster being paired with this release.
pub fn attribute(manifest: &mut Manifest, who: &Attribution) {
    manifest.machine_id = Some(who.machine_id.clone());
    manifest.roster_seq = Some(who.roster_seq);
}

#[cfg(test)]
#[path = "machines_tests.rs"]
mod tests;
