// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A root-authorized, device-sealed local intake. This domain carries no
//! account decision, membership, allocation, or execution permission.

use super::*;
use crate::commons::binding::Binding;
use ring::signature::{ED25519, Ed25519KeyPair, UnparsedPublicKey};

const MAGIC: &[u8; 4] = b"AKI1";
const DOMAIN: &[u8] = b"aterm/native-account-input/1\0";
const MAX: usize = 128 * 1024;

fn native_entry(name: &str) -> crate::Result<()> {
    if name == "ai-native-pool.toml"
        || (name.starts_with("native-api-key-")
            && name.len() == "native-api-key-".len() + 48
            && name["native-api-key-".len()..]
                .bytes()
                .all(|b| b.is_ascii_hexdigit()))
    {
        Ok(())
    } else {
        Err(Error::new("native account entry has another purpose"))
    }
}

/// Publish one native account input or allocation entry with the normal signed
/// vault version/CAS rules and one shared deadline. Other document state comes
/// only from the current opened Admin, never from a saved pending draft.
pub fn admin_set_entry_until(
    paths: &Paths,
    admin: &Admin,
    name: &str,
    expected: Option<[u8; 32]>,
    value: &[u8],
    deadline: Instant,
) -> crate::Result<Published> {
    publication_current(Some(deadline))?;
    let actual = admin.native_entry_digest(name)?;
    if actual != expected {
        return Err(Error::new(
            "native account entry changed before publication",
        ));
    }
    let mut doc = admin.published_doc.clone();
    doc.set_entry(&Audience::Everyone, name, value)
        .map_err(|e| Error::new(e.to_string()))?;
    publish_as_with(
        paths,
        admin,
        &doc,
        false,
        &[],
        false,
        &[],
        None,
        Some(deadline),
    )
}

impl Admin {
    /// Return the published baseline for one native preparation or allocation
    /// entry. Unpublished edits in this Admin are never used for a CAS receipt.
    pub fn native_entry_digest(&self, name: &str) -> crate::Result<Option<[u8; 32]>> {
        native_entry(name)?;
        Ok(self
            .published_doc
            .entry(&Audience::Everyone, name)
            .map(|entry| aterm_digest::Sha256::digest(entry.value.as_slice())))
    }

    /// Seal an explicitly authorized intake to one exact device binding. The
    /// caller retains these bytes durably before performing its first effect.
    /// A later consumer must recheck current root, binding, target and policy.
    pub fn seal_account_input(&self, device: &Binding, payload: &[u8]) -> crate::Result<Vec<u8>> {
        if payload.len() > MAX || device.authority() != &self.commons_authority()? {
            return Err(Error::new(
                "account input has another authority or exceeds its bound",
            ));
        }
        let sealed = crate::age::encrypt(&[device.recipient()], payload)?;
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&device.bytes());
        bytes.extend_from_slice(&(sealed.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&sealed);
        let mut signed = DOMAIN.to_vec();
        signed.extend_from_slice(&bytes);
        let signer = Ed25519KeyPair::from_seed_unchecked(&self.commons_seed)
            .map_err(|_| Error::new("account input signer is unavailable"))?;
        bytes.extend_from_slice(signer.sign(&signed).as_ref());
        Ok(bytes)
    }
}

/// Verify against the caller's current authenticated authority and exact
/// binding before decrypting. A signer named by untrusted input is never used.
/// The returned bytes authorize no effect without the application's current
/// target, source, policy and replay checks.
pub fn open_account_input(
    authority: &[u8; 32],
    device: &Binding,
    identity: &Identity,
    bytes: &[u8],
) -> crate::Result<SecretBytes> {
    let binding = device.bytes();
    let prefix = MAGIC.len() + binding.len() + 4;
    if device.authority() != authority
        || identity.recipient() != device.recipient()
        || bytes.len() < prefix + 64
        || bytes.len() > MAX + 4096
        || &bytes[..4] != MAGIC
        || bytes[4..4 + binding.len()] != binding
    {
        return Err(Error::new(
            "account input has another root, binding or frame",
        ));
    }
    let size = u32::from_le_bytes(bytes[prefix - 4..prefix].try_into().unwrap()) as usize;
    if size > MAX + 1024 || prefix + size + 64 != bytes.len() {
        return Err(Error::new("account input frame length is invalid"));
    }
    let mut signed = DOMAIN.to_vec();
    signed.extend_from_slice(&bytes[..prefix + size]);
    UnparsedPublicKey::new(&ED25519, authority)
        .verify(&signed, &bytes[prefix + size..])
        .map_err(|_| Error::new("account input root authorization is invalid"))?;
    let plain = crate::age::decrypt(
        std::slice::from_ref(identity),
        &bytes[prefix..prefix + size],
    )
    .map_err(|_| Error::new("account input cannot be opened by this device"))?;
    if plain.len() > MAX {
        return Err(Error::new("account input exceeds its bound"));
    }
    Ok(plain)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (aterm_tempfile::TempDir, Paths, Admin) {
        let temp = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(&temp.path().join("home"));
        super::super::create(
            &paths,
            temp.path().join("test.vault").to_str().unwrap(),
            None,
            Some("m1"),
        )
        .unwrap();
        let admin = super::super::admin_open(&paths).unwrap();
        (temp, paths, admin)
    }

    #[test]
    fn native_entry_publication_preserves_other_entries_and_compares_current_value() {
        let (_temp, paths, mut admin) = fixture();
        let name = format!("native-api-key-{}", "11".repeat(24));
        let before = admin.doc.entries();
        admin
            .doc
            .set_entry(&Audience::Everyone, "unpublished-draft", b"must-not-escape")
            .unwrap();
        admin
            .doc
            .set_entry(&Audience::Everyone, &name, b"unpublished-native-draft")
            .unwrap();
        assert_eq!(admin.native_entry_digest(&name).unwrap(), None);
        admin_set_entry_until(
            &paths,
            &admin,
            &name,
            None,
            b"first",
            Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        )
        .unwrap();
        let current = super::super::admin_open(&paths).unwrap();
        assert!(
            current
                .doc
                .entry(&Audience::Everyone, "unpublished-draft")
                .is_none()
        );
        for entry in before {
            assert_eq!(
                current.doc.entry(&entry.audience, &entry.name),
                Some(&entry)
            );
        }
        assert_eq!(
            current
                .doc
                .entry(&Audience::Everyone, &name)
                .unwrap()
                .value
                .as_slice(),
            b"first"
        );
        assert!(
            admin_set_entry_until(
                &paths,
                &current,
                &name,
                None,
                b"wrong",
                Instant::now() + aterm_spec::wait::MUST_HAPPEN
            )
            .is_err()
        );
        assert_eq!(
            super::super::admin_open(&paths)
                .unwrap()
                .doc
                .entry(&Audience::Everyone, &name)
                .unwrap()
                .value
                .as_slice(),
            b"first"
        );
    }

    #[test]
    fn expired_native_publication_mutates_no_entry() {
        let (_temp, paths, admin) = fixture();
        let before = admin.doc.render();
        let name = format!("native-api-key-{}", "22".repeat(24));
        assert!(
            admin_set_entry_until(&paths, &admin, &name, None, b"never", Instant::now()).is_err()
        );
        assert_eq!(
            super::super::admin_open(&paths).unwrap().doc.render(),
            before
        );
        assert!(!paths.home.join(".secrets").join(name).exists());
    }

    #[test]
    fn native_publication_waits_on_its_existing_state_lock_within_one_budget() {
        let (_temp, paths, admin) = fixture();
        let held = std::sync::Arc::new(std::sync::Mutex::new(Some(
            super::super::StateLock::take(&paths).unwrap(),
        )));
        let release = held.clone();
        let observed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let seen = observed.clone();
        let name = format!("native-api-key-{}", "33".repeat(24));
        aterm_uds::private_file::with_lock_wait_observer(
            move |_| {
                seen.store(true, std::sync::atomic::Ordering::Relaxed);
                release.lock().unwrap().take();
            },
            || {
                admin_set_entry_until(
                    &paths,
                    &admin,
                    &name,
                    None,
                    b"after contention",
                    Instant::now() + aterm_spec::wait::MUST_HAPPEN,
                )
                .unwrap();
            },
        );
        assert!(observed.load(std::sync::atomic::Ordering::Relaxed));
        assert!(held.lock().unwrap().is_none());
    }
}
