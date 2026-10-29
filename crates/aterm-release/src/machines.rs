// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The producer half of the machine-roster tier: this machine's public identity, the cut-time gate that proves it is still authorized, and the stamp that puts attribution inside the signed appcast bytes.

use std::path::{Path, PathBuf};

use aterm_update_core::Manifest;
use aterm_update_core::roster::{Attribution, Roster, verify_roster};

use crate::ledger::{Error, Result};

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
        if !path.exists() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::new(format!("read {}: {e}", path.display())))?;
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
        let bytes = std::fs::read(roster).map_err(|e| {
            Error::new(format!(
                "read the machine roster {}: {e} — `machine_roster` in the \
                 release-credentials profile names it, so the cut cannot proceed without it",
                roster.display()
            ))
        })?;
        let signature = std::fs::read(&signature_path).map_err(|e| {
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

/// The master-admitted roster on the public channel's latest release: its `roster_seq` and verified bytes, or `None` when that release carries no roster.
pub(crate) fn channel_roster_document(
    slug: &str,
) -> std::result::Result<Option<(u64, Vec<u8>)>, String> {
    let url = |asset: &str| format!("https://github.com/{slug}/releases/latest/download/{asset}");
    let bytes = match anonymous_fetch(&url(aterm_update_core::roster::ROSTER_ASSET), 65_536) {
        Ok(b) => b,
        Err(e) if e.contains("returned error: 404") => return Ok(None),
        Err(e) => return Err(e),
    };
    let sig = anonymous_fetch(&url(aterm_update_core::roster::ROSTER_SIG_ASSET), 4_096)?;
    let verified = verify_roster(aterm_update_core::pins::PAPER_MASTER_PUBKEYS, bytes, &sig)
        .map_err(|e| {
            format!("the channel roster did not verify under the committed paper master ({e:?})")
        })?;
    let parsed = Roster::parse(&verified)
        .map_err(|e| format!("the channel roster verified but did not parse ({e:?})"))?;
    Ok(Some((parsed.roster_seq, verified.as_slice().to_vec())))
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

/// One bounded anonymous download, under the client's own roster limits (64 KiB body, 4 KiB
/// signature): a floor read never accepts more than the fleet would.
fn anonymous_fetch(url: &str, cap: usize) -> std::result::Result<Vec<u8>, String> {
    let cap_s = cap.to_string();
    let out = std::process::Command::new(aterm_uds::os_path!("curl"))
        .args([
            "-fsSL",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--max-time",
            "60",
            "--max-filesize",
            &cap_s,
            url,
        ])
        .output()
        .map_err(|e| format!("failed to run curl: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("curl {url}: {}", err.trim()));
    }
    if out.stdout.len() > cap {
        return Err(format!("{url}: exceeded the {cap}-byte cap"));
    }
    if out.stdout.is_empty() {
        return Err(format!("{url}: zero bytes"));
    }
    Ok(out.stdout)
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
    Ok(roster.roster_seq)
}

/// Stamp attribution into the manifest before it is serialized and signed: `machine_id` makes the signature unrelabellable and `roster_seq` stops an old roster being paired with this release.
pub fn attribute(manifest: &mut Manifest, who: &Attribution) {
    manifest.machine_id = Some(who.machine_id.clone());
    manifest.roster_seq = Some(who.roster_seq);
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Standard padded Base64 — the shipped encoder (`aterm_codec::base64`).
    fn b64(raw: &[u8]) -> String {
        aterm_codec::base64::encode(raw).expect("test key material is far below MAX_INPUT_LEN")
    }
    use ring::signature::{Ed25519KeyPair, KeyPair};

    const MASTER: [u8; 32] = [0x51; 32];
    const M3: [u8; 32] = [0x52; 32];
    const M11: [u8; 32] = [0x53; 32];

    fn kp(seed: &[u8; 32]) -> Ed25519KeyPair {
        Ed25519KeyPair::from_seed_unchecked(seed).unwrap()
    }

    fn pk(seed: &[u8; 32]) -> String {
        b64(kp(seed).public_key().as_ref())
    }

    /// A roster listing m3 and m11, with `revoked` under the caller's control.
    fn roster(revoked: &[&str]) -> (Vec<u8>, Vec<u8>, String) {
        let r = Roster {
            schema: 1,
            roster_seq: 6,
            valid_until: "2027-02-01T00:00:00Z".into(),
            machines: vec![
                aterm_update_core::roster::Machine {
                    id: "m3".into(),
                    pubkey: pk(&M3),
                    added_at: "2026-08-04T00:00:00Z".into(),
                    not_after: None,
                },
                aterm_update_core::roster::Machine {
                    id: "m11".into(),
                    pubkey: pk(&M11),
                    added_at: "2026-08-04T00:00:00Z".into(),
                    not_after: None,
                },
            ],
            revoked: revoked.iter().map(|s| (*s).to_string()).collect(),
        };
        let bytes = r.to_toml().unwrap().into_bytes();
        let sig = kp(&MASTER).sign(&bytes).as_ref().to_vec();
        (bytes, sig, pk(&MASTER))
    }

    /// The fixture roster's `valid_until`, 2027-02-01T00:00:00Z, as unix seconds.
    const FIXTURE_VALID_UNTIL: i64 = 1_801_440_000;

    /// A roster still valid but within [`MIN_REMAINING_WINDOW_SECS`] of lapsing may not start a
    /// cut: pins admitting at the horizon, not at `now`.
    #[test]
    fn a_roster_about_to_lapse_may_not_start_a_cut() {
        let one_second_left = FIXTURE_VALID_UNTIL - 1;
        let (bytes, sig, master) = roster(&[]);
        authorize_cut(&[&master], bytes, &sig, &pk(&M3), one_second_left).unwrap_err();
        let (bytes, sig, master) = roster(&[]);
        let verified = verify_roster(&[&master], bytes, &sig).unwrap();
        Roster::parse(&verified)
            .unwrap()
            .admit(0, one_second_left)
            .expect("the client's bound is still open — only the producer looks ahead");

        let just_short = FIXTURE_VALID_UNTIL - MIN_REMAINING_WINDOW_SECS;
        let (bytes, sig, master) = roster(&[]);
        assert!(authorize_cut(&[&master], bytes, &sig, &pk(&M3), just_short).is_err());
        let (bytes, sig, master) = roster(&[]);
        assert!(authorize_cut(&[&master], bytes, &sig, &pk(&M3), just_short - 1).is_ok());
    }

    /// Two master-signed documents at the same generation are a lineage fork, which the number
    /// admits, so the bytes decide.
    #[test]
    fn an_equal_generation_with_a_different_document_is_a_lineage_fork() {
        let ours = b"roster_seq = 4\n[[machine]]\nid = \"m3\"\n".to_vec();
        let theirs = b"roster_seq = 4\n[[machine]]\nid = \"m19\"\n".to_vec();
        assert!(roster_lineage_agrees(&ours, Some(4), Some(&(4, ours.clone()))).is_ok());
        roster_lineage_agrees(&ours, Some(4), Some(&(4, theirs.clone()))).unwrap_err();
        assert!(roster_lineage_agrees(&ours, Some(4), Some(&(3, theirs.clone()))).is_ok());
        assert!(roster_lineage_agrees(&ours, Some(3), Some(&(4, theirs))).is_ok());
        assert!(roster_lineage_agrees(&ours, Some(4), None).is_ok());
        assert!(roster_lineage_agrees(&ours, None, Some(&(4, ours.clone()))).is_ok());
    }

    /// A recovered roster must be the one the published manifest names; freshness and
    /// revocation are not re-judged.
    #[test]
    fn a_recovered_roster_is_bound_to_the_manifest_that_names_it() {
        let (bytes, sig, master) = roster(&[]);
        verify_published_roster(&[&master], bytes.clone(), &sig, "m3", Some(6))
            .expect("the roster this manifest names");
        let lapsed = Roster::parse(&verify_roster(&[&master], bytes.clone(), &sig).unwrap())
            .unwrap()
            .admit(0, 1_900_000_000);
        assert!(lapsed.is_err(), "precondition: these bytes are lapsed");
        verify_published_roster(&[&master], bytes.clone(), &sig, "m3", Some(6))
            .expect("recovery does not re-judge a window it cannot change");

        verify_published_roster(&[&master], bytes.clone(), &sig, "m3", Some(7)).unwrap_err();
        verify_published_roster(&[&master], bytes.clone(), &sig, "m3", None).unwrap_err();
        verify_published_roster(&[&master], bytes.clone(), &sig, "m3", Some(5))
            .expect("a newer roster asset over an older attribution is what a join produces");
        verify_published_roster(&[&master], bytes.clone(), &sig, "m99", Some(6)).unwrap_err();
        verify_published_roster(&[], bytes, &sig, "m3", Some(6)).unwrap_err();
    }

    #[test]
    fn a_missing_identity_is_none_and_a_broken_one_is_an_error() {
        let dir = std::env::temp_dir().join(format!("aterm-machine-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("machine.toml");
        assert_eq!(MachineIdentity::read(&path).unwrap(), None);

        std::fs::write(&path, "id = \"m3\"\npubkey = \"abc\"\nminted_at = \"x\"\n").unwrap();
        let me = MachineIdentity::read(&path).unwrap().unwrap();
        assert_eq!(me.id, "m3");
        assert_eq!(me.pubkey, "abc");

        std::fs::write(&path, "id = \"\"\npubkey = \"abc\"\n").unwrap();
        assert!(
            MachineIdentity::read(&path).is_err(),
            "an empty id is not an identity"
        );

        std::fs::write(&path, "this is not toml = = =").unwrap();
        assert!(MachineIdentity::read(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Release attribution reads `~/.aterm/release-ring/machine.toml` and ignores the host's own
    /// `~/.aterm/machine.toml` (`id != m22` in 0.146).
    /// NEGATIVE CONTROL: reading `.aterm/machine.toml` returns `"m99"` instead of `"m22"` (or
    /// `Some("m99")` when the release ring is absent).
    #[test]
    fn release_ring_identity_path_ignores_host_machine_toml() {
        let home = std::env::temp_dir().join(format!("aterm-ring-id-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".aterm")).unwrap();
        std::fs::write(
            home.join(".aterm/machine.toml"),
            "id = \"m99\"\npubkey = \"host-key\"\n",
        )
        .unwrap();

        let ring_record = identity_path_in(&home);
        assert_eq!(
            declared_machine_id(None, Some(&ring_record)).unwrap(),
            None,
            "host machine.toml alone never supplies a release machine id"
        );

        std::fs::create_dir_all(ring_record.parent().unwrap()).unwrap();
        std::fs::write(&ring_record, "id = \"m22\"\npubkey = \"ring-key\"\n").unwrap();
        assert_eq!(
            declared_machine_id(None, Some(&ring_record)).unwrap(),
            Some("m22".to_string()),
            "release-ring/machine.toml wins over ~/.aterm/machine.toml"
        );
        let _ = std::fs::remove_dir_all(&home);
    }
}
