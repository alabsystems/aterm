// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Independent canonical membership high-water; no principal/token inventory.
//! Replaying the mutable manifest alone cannot remove an admitted operation.
use super::*;
const NAME: &str = "owner-catalogue-witness-v1";
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Head {
    namespace: String,
    sequence: u64,
    digest: String,
}
impl Head {
    fn valid(&self) -> bool {
        valid_hex(&self.namespace, 64) && self.sequence > 0 && valid_hex(&self.digest, 64)
    }
    fn of(manifest: &Manifest) -> Result<Self, String> {
        let bytes = aterm_json::to_vec(manifest).map_err(|_| "catalogue manifest invalid")?;
        Ok(Self {
            namespace: manifest.namespace.clone(),
            sequence: manifest.sequence,
            digest: aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(&bytes)),
        })
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Witness {
    schema: u32,
    current: Option<Head>,
    pending: Option<Head>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    witness: Witness,
    mac: String,
}
pub(super) struct MembershipAnchor {
    store: platform::Store,
    witness: Witness,
}
impl MembershipAnchor {
    pub(super) fn open(
        parent: &Path,
        home: &Path,
        identity: &age::Identity,
        fresh: bool,
    ) -> Result<Self, String> {
        let path = parent.join(NAME);
        let (store, bytes) = if fresh {
            platform::Store::open_owned(&path, home, 16 * 1024)?
        } else {
            platform::Store::open_owned_existing(&path, home, 16 * 1024)?
                .ok_or("catalogue membership witness absent")?
        };
        if store.recipient() != identity.recipient() {
            return Err("catalogue membership identity changed".into());
        }
        let witness = match bytes {
            Some(bytes) => {
                let envelope: Envelope = aterm_json::from_slice(bytes.as_slice())
                    .map_err(|_| "catalogue membership witness invalid")?;
                let payload = aterm_json::to_vec(&envelope.witness)
                    .map_err(|_| "catalogue membership witness invalid")?;
                let mac = machine_mac(
                    identity,
                    b"aterm/codex-catalogue/membership/key/v1\0",
                    &payload,
                )?;
                if !valid(&envelope.witness)
                    || !aterm_digest::ct_eq(mac.as_bytes(), envelope.mac.as_bytes())
                {
                    return Err("catalogue membership witness invalid".into());
                }
                envelope.witness
            }
            None if fresh => Witness {
                schema: 1,
                current: None,
                pending: None,
            },
            None => return Err("catalogue membership witness lost".into()),
        };
        Ok(Self { store, witness })
    }
    pub(super) fn bootstrap_manifest(&self, directory: &Path) -> Result<Option<Manifest>, String> {
        let Some(pending) = self.witness.pending.as_ref() else {
            return Ok(None);
        };
        if self.witness.current.is_some() || pending.sequence != 1 {
            return Ok(None);
        }
        // Initial membership has a single deterministic empty payload. Its
        // already durable canonical digest authenticates this exact namespace
        // and physical directory; no current roster/selection is consulted.
        platform::check_private_dir(directory)
            .map_err(|_| "catalogue bootstrap directory unavailable")?;
        let manifest = Manifest {
            schema: 1,
            namespace: pending.namespace.clone(),
            directory: codex_enrollment::DirectoryIdentity::capture(directory)?,
            sequence: 1,
            members: BTreeMap::new(),
            unattempted: Default::default(),
        };
        if Head::of(&manifest)? != *pending {
            return Err("catalogue bootstrap physical identity changed".into());
        }
        if std::fs::read_dir(directory)
            .map_err(|_| "catalogue bootstrap directory unavailable")?
            .next()
            .is_some()
        {
            return Err("catalogue bootstrap has unexplained custody".into());
        }
        self.store.unchanged().map_err(str::to_owned)?;
        Ok(Some(manifest))
    }
    fn save(&mut self, identity: &age::Identity, witness: Witness) -> Result<(), String> {
        if !valid(&witness) {
            return Err("catalogue membership transition invalid".into());
        }
        let payload =
            aterm_json::to_vec(&witness).map_err(|_| "catalogue membership witness invalid")?;
        let mac = machine_mac(
            identity,
            b"aterm/codex-catalogue/membership/key/v1\0",
            &payload,
        )?;
        let bytes = aterm_json::to_vec(&Envelope {
            witness: witness.clone(),
            mac,
        })
        .map_err(|_| "catalogue membership witness invalid")?;
        persist_exact(&mut self.store, identity, &bytes)?;
        self.witness = witness;
        Ok(())
    }
    pub(super) fn observe(
        &mut self,
        identity: &age::Identity,
        manifest: &Manifest,
    ) -> Result<(), String> {
        let actual = Head::of(manifest)?;
        if self.witness.pending.as_ref() == Some(&actual) {
            self.save(
                identity,
                Witness {
                    schema: 1,
                    current: Some(actual),
                    pending: None,
                },
            )?;
        }
        self.current(manifest)
    }
    pub(super) fn current(&self, manifest: &Manifest) -> Result<(), String> {
        if self.witness.pending.is_some()
            || self.witness.current.as_ref() != Some(&Head::of(manifest)?)
        {
            return Err("catalogue membership requires exact reconciliation".into());
        }
        self.store.unchanged().map_err(str::to_owned)
    }
    pub(super) fn prepare_after(
        &mut self,
        identity: &age::Identity,
        previous: &Manifest,
        next: &Manifest,
    ) -> Result<(), String> {
        if self.witness.current.as_ref() != Some(&Head::of(previous)?) {
            return Err("catalogue pending proposal has another predecessor".into());
        }
        self.prepare(identity, next)
    }
    pub(super) fn prepare(
        &mut self,
        identity: &age::Identity,
        manifest: &Manifest,
    ) -> Result<(), String> {
        let next = Head::of(manifest)?;
        if self.witness.pending.as_ref() == Some(&next) {
            return Ok(());
        }
        if self.witness.pending.is_some()
            || self.witness.current.as_ref().is_some_and(|old| {
                old.namespace != next.namespace
                    || old.sequence.checked_add(1) != Some(next.sequence)
            })
        {
            return Err("catalogue membership transition changed".into());
        }
        self.save(
            identity,
            Witness {
                schema: 1,
                current: self.witness.current.clone(),
                pending: Some(next),
            },
        )
    }
}
fn valid(witness: &Witness) -> bool {
    witness.schema == 1
        && witness.current.as_ref().is_none_or(Head::valid)
        && witness.pending.as_ref().is_none_or(Head::valid)
        && (witness.current.is_some() || witness.pending.is_some())
        && match (&witness.current, &witness.pending) {
            (Some(old), Some(next)) => {
                old.namespace == next.namespace
                    && old.sequence.checked_add(1) == Some(next.sequence)
            }
            (None, Some(next)) => next.sequence == 1,
            _ => true,
        }
}

/// Stable observation of the independent high-water. It cannot prepare,
/// observe/roll-forward, or save a membership transition.
pub(super) struct MembershipObservation {
    file: super::observation_file::CipherObservation,
    head: Head,
}
impl MembershipObservation {
    pub(super) fn read(
        parent: &Path,
        identity: &age::Identity,
        manifest: &Manifest,
    ) -> Result<Self, String> {
        let (file, cipher) =
            super::observation_file::CipherObservation::read(&parent.join(NAME), 16 * 1024)?;
        let bytes = age::decrypt(std::slice::from_ref(identity), cipher.as_slice())
            .map_err(|_| "catalogue membership witness invalid")?;
        let envelope: Envelope = aterm_json::from_slice(bytes.as_slice())
            .map_err(|_| "catalogue membership witness invalid")?;
        let payload = aterm_json::to_vec(&envelope.witness)
            .map_err(|_| "catalogue membership witness invalid")?;
        let mac = machine_mac(
            identity,
            b"aterm/codex-catalogue/membership/key/v1\0",
            &payload,
        )?;
        if !valid(&envelope.witness)
            || envelope.witness.pending.is_some()
            || !aterm_digest::ct_eq(mac.as_bytes(), envelope.mac.as_bytes())
            || envelope.witness.current.as_ref() != Some(&Head::of(manifest)?)
        {
            return Err("catalogue membership requires exact reconciliation".into());
        }
        let observed = Self {
            file,
            head: envelope.witness.current.expect("checked committed head"),
        };
        observed.current(manifest)?;
        Ok(observed)
    }
    pub(super) fn current(&self, manifest: &Manifest) -> Result<(), String> {
        if self.head != Head::of(manifest)? {
            return Err("catalogue membership changed".into());
        }
        self.file.unchanged().map_err(str::to_owned)
    }
}
