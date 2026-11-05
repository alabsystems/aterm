// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Verified, content-addressed release images for independent service activation.

pub use aterm_update_core::service_image::{
    ProtocolRange, Qualification, ServiceDeclaration, image_digest, valid_executable_path,
};
use aterm_update_core::{FileLock, Manifest, Roster, ensure_private_dir, sha256_file};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

#[cfg(target_os = "macos")]
const IMAGE_NAME: &str = "image.app";
#[cfg(not(target_os = "macos"))]
const IMAGE_NAME: &str = "image";

/// Exact signed release evidence, retained for re-verification and revocation checks.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedRelease {
    /// Exact appcast bytes, including its service declaration.
    pub manifest: String,
    /// Detached Ed25519 signature.
    pub manifest_signature: Vec<u8>,
    /// Exact master-signed roster bytes.
    pub roster: Vec<u8>,
    /// Detached roster signature.
    pub roster_signature: Vec<u8>,
}

/// Image identity whose authenticity has been rechecked by the store.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    /// Release signer authenticated by the master-signed roster.
    pub signer: String,
    /// Monotonic release build.
    pub build: u64,
    /// Full source revision.
    pub revision: String,
    /// Signed service declaration.
    pub service: ServiceDeclaration,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    signed: SignedRelease,
    qualification: Vec<u8>,
}
#[derive(Serialize, Deserialize)]
struct Extension {
    service: ServiceDeclaration,
}

fn hex(bytes: &[u8]) -> String {
    aterm_codec::hex::encode_lower(bytes)
}
fn digest(bytes: &[u8]) -> String {
    hex(&aterm_digest::Sha256::digest(bytes))
}
fn valid_hash(s: &str) -> bool {
    aterm_codec::hex::is_lower(s, 64)
}
fn relative(path: &Path) -> bool {
    valid_executable_path(path)
}
fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Admit a fresh service-repair roster into the GUI updater's durable revocation floor.
/// Returns the resulting roster generation only after the durable write succeeds.
#[cfg(target_os = "macos")]
pub fn admit_current_roster(roster: &[u8], signature: &[u8], now: i64) -> Result<u64, String> {
    let staging = crate::paths::Staging::resolve().ok_or("updater state is unavailable")?;
    let floor = crate::manifest::Floor::read_for_gate(&staging.floor())?;
    let verified = aterm_update_core::verify_roster(
        aterm_update_core::pins::PAPER_MASTER_PUBKEYS,
        roster.to_vec(),
        signature,
    )
    .map_err(|e| format!("service roster: {e:?}"))?;
    if verified.master_index() != 0 {
        return Err("service roster was not signed by the current master".into());
    }
    let roster = Roster::parse(&verified).map_err(|e| format!("service roster: {e:?}"))?;
    roster
        .admit(floor.roster_seq, now)
        .map_err(|e| format!("service roster: {e:?}"))?;
    crate::manifest::Floor::observe_reporting(
        &staging.floor(),
        &crate::manifest::FloorObservation {
            roster_seq: roster.roster_seq,
            revoked: &roster.revoked,
            ..crate::manifest::FloorObservation::default()
        },
    )?;
    Ok(crate::manifest::Floor::read_for_gate(&staging.floor())?.roster_seq)
}

/// Check an authenticated receipt against the GUI updater's current floor and revocations.
#[cfg(target_os = "macos")]
pub fn check_current_floor(receipt: &Receipt) -> Result<(), String> {
    let staging = crate::paths::Staging::resolve().ok_or("updater state is unavailable")?;
    let floor = crate::manifest::Floor::read_for_gate(&staging.floor())?;
    match floor.withdraws(receipt.build, Some(&receipt.signer)) {
        Some(why) => Err(why),
        None => Ok(()),
    }
}

#[cfg(target_os = "macos")]
fn current_roster_floor() -> Result<u64, String> {
    let staging = crate::paths::Staging::resolve().ok_or("updater state is unavailable")?;
    Ok(crate::manifest::Floor::read_for_gate(&staging.floor())?.roster_seq)
}

fn authenticate(
    signed: &SignedRelease,
    report: &[u8],
    now: i64,
    floor: u64,
    keys: &[&str],
) -> Result<Receipt, String> {
    let verified =
        aterm_update_core::verify_roster(keys, signed.roster.clone(), &signed.roster_signature)
            .map_err(|e| format!("service roster: {e:?}"))?;
    if verified.master_index() != 0 {
        return Err("service roster was not signed by the current master".into());
    }
    let roster = Roster::parse(&verified).map_err(|e| format!("service roster: {e:?}"))?;
    roster
        .admit(floor, now)
        .map_err(|e| format!("service roster: {e:?}"))?;
    let who = roster
        .authorize_appcast(signed.manifest.as_bytes(), &signed.manifest_signature, now)
        .map_err(|e| format!("service release: {e:?}"))?;
    let manifest = Manifest::parse(&signed.manifest)?;
    who.bind(manifest.machine_id.as_deref(), manifest.roster_seq)
        .map_err(|e| format!("service attribution: {e:?}"))?;
    let Extension { service } = aterm_toml::from_str(&signed.manifest).map_err(error)?;
    let revision = manifest
        .commit
        .filter(|s| aterm_codec::hex::is_lower(s, 40))
        .ok_or("service release requires full source revision")?;
    if service.schema != 1
        || service.platform.trim().is_empty()
        || !relative(&service.executable)
        || !service.protocol.supports(&[])
        || !valid_hash(&service.image_sha256)
        || !valid_hash(&service.executable_sha256)
        || !valid_hash(&service.qualification_sha256)
        || digest(report) != service.qualification_sha256
        || manifest.build_number == 0
    {
        return Err("invalid signed service declaration or qualification report".into());
    }
    let qualification: Qualification = aterm_json::from_slice(report).map_err(error)?;
    qualification.validate(manifest.build_number, &revision, &service)?;
    Ok(Receipt {
        signer: who.machine_id,
        build: manifest.build_number,
        revision,
        service,
    })
}

fn check_no_symlink_ancestors(root: &Path) -> Result<(), String> {
    for ancestor in root.ancestors() {
        if let Ok(meta) = std::fs::symlink_metadata(ancestor)
            && meta.file_type().is_symlink()
        {
            return Err("service store must not traverse symlinks".into());
        }
    }
    Ok(())
}

/// Durable image store; its root must be an application state directory, never a cache.
pub struct Store {
    root: PathBuf,
}
impl Store {
    /// Open a private, permanent service store. No image is overwritten in place.
    pub fn new(root: PathBuf) -> Result<Self, String> {
        if !root.is_absolute()
            || root.components().any(|c| match c {
                Component::Normal(v) => {
                    let v = v.to_string_lossy().to_ascii_lowercase();
                    matches!(
                        v.as_str(),
                        "caches" | "cache" | "target" | "tmp" | "temp" | "folders"
                    ) || v.ends_with(".noindex")
                }
                Component::ParentDir => true,
                _ => false,
            })
        {
            return Err("service store must be permanent absolute application state".into());
        }
        check_no_symlink_ancestors(&root)?;
        ensure_private_dir(&root).map_err(error)?;
        if root.canonicalize().map_err(error)? != root {
            return Err("service store must not traverse symlinks".into());
        }
        Ok(Self { root })
    }
    /// The content-addressed executable, after [`Self::verify`] succeeds.
    pub fn executable(&self, receipt: &Receipt) -> Result<PathBuf, String> {
        if !valid_hash(&receipt.service.image_sha256) || !relative(&receipt.service.executable) {
            return Err("invalid service executable identity".into());
        }
        Ok(self
            .root
            .join(&receipt.service.image_sha256)
            .join(IMAGE_NAME)
            .join(&receipt.service.executable))
    }
    /// Copy a complete extracted release image into durable storage and verify it again.
    /// The extraction input is never mutated; all authentication uses the signed appcast.
    pub fn materialize(
        &self,
        signed: &SignedRelease,
        image: &Path,
        qualification: &[u8],
        now: i64,
        roster_floor: u64,
    ) -> Result<Receipt, String> {
        #[cfg(target_os = "macos")]
        let roster_floor = roster_floor.max(admit_current_roster(
            &signed.roster,
            &signed.roster_signature,
            now,
        )?);
        let receipt = self.materialize_with(
            signed,
            image,
            qualification,
            now,
            roster_floor,
            aterm_update_core::pins::PAPER_MASTER_PUBKEYS,
        )?;
        #[cfg(target_os = "macos")]
        self.check_native(&receipt)?;
        Ok(receipt)
    }
    fn materialize_with(
        &self,
        signed: &SignedRelease,
        image: &Path,
        qualification: &[u8],
        now: i64,
        floor: u64,
        keys: &[&str],
    ) -> Result<Receipt, String> {
        let receipt = authenticate(signed, qualification, now, floor, keys)?;
        check_image(image, &receipt)?;
        let _lock = FileLock::acquire(&self.root.join("store.lock")).map_err(error)?;
        let final_dir = self.root.join(&receipt.service.image_sha256);
        match std::fs::symlink_metadata(&final_dir) {
            Ok(meta) if meta.file_type().is_dir() => {
                self.record(&receipt.service.image_sha256)?;
                let checked = self.check_stored(&receipt.service.image_sha256, receipt.clone())?;
                let bytes = aterm_json::to_vec(&Record {
                    signed: signed.clone(),
                    qualification: qualification.to_vec(),
                })
                .map_err(error)?;
                refresh_record(&final_dir.join("receipt.json"), &bytes)?;
                aterm_durable::Flush::Durable
                    .dir(&self.root)
                    .map_err(error)?;
                return Ok(checked);
            }
            Ok(_) => return Err("stored service image entry must be a real directory".into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(error(e)),
        }
        let scratch = self.root.join(format!(
            ".materialize-{}",
            aterm_uds::rand::hex_token::<16>().map_err(error)?
        ));
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&scratch)
                .map_err(error)?;
        }
        #[cfg(not(unix))]
        std::fs::create_dir(&scratch).map_err(error)?;
        let result = (|| {
            ensure_private_dir(&scratch).map_err(error)?;
            copy_tree(image, &scratch.join(IMAGE_NAME))?;
            check_image(&scratch.join(IMAGE_NAME), &receipt)?;
            let record = Record {
                signed: signed.clone(),
                qualification: qualification.to_vec(),
            };
            let bytes = aterm_json::to_vec(&record).map_err(error)?;
            aterm_uds::private_file::write_private(&scratch, "receipt.json", &bytes)
                .map_err(error)?;
            std::fs::rename(&scratch, &final_dir).map_err(error)?;
            aterm_durable::Flush::Durable
                .dir(&self.root)
                .map_err(error)?;
            Ok(receipt)
        })();
        if result.is_err() {
            let _ = std::fs::remove_dir_all(&scratch);
        }
        result
    }
    /// Revalidate signature, current roster floor, qualification and every stored image byte.
    pub fn verify(
        &self,
        image_sha256: &str,
        now: i64,
        roster_floor: u64,
    ) -> Result<Receipt, String> {
        #[cfg(target_os = "macos")]
        let roster_floor = roster_floor.max(current_roster_floor()?);
        let receipt = self.verify_with(
            image_sha256,
            now,
            roster_floor,
            aterm_update_core::pins::PAPER_MASTER_PUBKEYS,
        )?;
        #[cfg(target_os = "macos")]
        self.check_native(&receipt)?;
        Ok(receipt)
    }
    /// Reauthorize a stored release under a freshly fetched master-signed roster.
    /// A newer roster can revoke the old signer or retain its authority without requiring
    /// the release itself to be signed again.
    pub fn verify_against(
        &self,
        image_sha256: &str,
        roster: &[u8],
        roster_signature: &[u8],
        now: i64,
        roster_floor: u64,
    ) -> Result<Receipt, String> {
        #[cfg(target_os = "macos")]
        let roster_floor = roster_floor.max(admit_current_roster(roster, roster_signature, now)?);
        let mut record = self.record(image_sha256)?;
        record.signed.roster = roster.to_vec();
        record.signed.roster_signature = roster_signature.to_vec();
        let receipt = authenticate(
            &record.signed,
            &record.qualification,
            now,
            roster_floor,
            aterm_update_core::pins::PAPER_MASTER_PUBKEYS,
        )?;
        let receipt = self.check_stored(image_sha256, receipt)?;
        #[cfg(target_os = "macos")]
        self.check_native(&receipt)?;
        Ok(receipt)
    }
    #[cfg(target_os = "macos")]
    fn check_native(&self, receipt: &Receipt) -> Result<(), String> {
        check_current_floor(receipt)?;
        if receipt.service.executable != Path::new("Contents/MacOS/aterm") {
            return Err("service image must name the shipped aterm executable".into());
        }
        let image = self
            .root
            .join(&receipt.service.image_sha256)
            .join(IMAGE_NAME);
        let checked = crate::verify::check_bundle(&image, crate::effective_team_id(), false);
        checked.policy?;
        if checked.build? != receipt.build
            || !crate::commit_matches(&checked.commit?, &receipt.revision)
        {
            return Err("sealed service image does not match signed release identity".into());
        }
        Ok(())
    }
    fn verify_with(
        &self,
        id: &str,
        now: i64,
        floor: u64,
        keys: &[&str],
    ) -> Result<Receipt, String> {
        let record = self.record(id)?;
        let receipt = authenticate(&record.signed, &record.qualification, now, floor, keys)?;
        self.check_stored(id, receipt)
    }
    fn record(&self, id: &str) -> Result<Record, String> {
        if !valid_hash(id) {
            return Err("invalid service image identity".into());
        }
        let dir = self.root.join(id);
        let bytes = aterm_uds::private_file::read_private(&dir, "receipt.json", 4 * 1024 * 1024)
            .map_err(error)?;
        aterm_json::from_slice(&bytes).map_err(error)
    }
    fn check_stored(&self, id: &str, receipt: Receipt) -> Result<Receipt, String> {
        if receipt.service.image_sha256 != id {
            return Err("service image address mismatch".into());
        }
        check_image(&self.root.join(id).join(IMAGE_NAME), &receipt)?;
        Ok(receipt)
    }
}

fn refresh_record(path: &Path, bytes: &[u8]) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        crate::manifest::write_durable(
            path,
            std::str::from_utf8(bytes).map_err(error)?,
            "service image receipt",
            aterm_durable::Flush::Durable,
        )?;
        aterm_durable::Flush::Durable
            .dir(path.parent().ok_or("receipt has no parent")?)
            .map_err(error)
    }
    #[cfg(target_os = "linux")]
    {
        crate::linux::write_atomic(path, bytes)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (path, bytes);
        Err("service receipt refresh is unsupported on this platform".into())
    }
}

fn check_image(image: &Path, receipt: &Receipt) -> Result<(), String> {
    if !relative(&receipt.service.executable) {
        return Err("invalid service executable identity".into());
    }
    let mut cursor = image.to_path_buf();
    let mut components = receipt.service.executable.components().peekable();
    while let Some(component) = components.next() {
        cursor.push(component);
        let meta = std::fs::symlink_metadata(&cursor).map_err(error)?;
        if components.peek().is_some() {
            if !meta.is_dir() {
                return Err("service executable parent must be a real directory".into());
            }
        } else if !meta.is_file() {
            return Err("service executable must be a regular file".into());
        }
    }
    if image_digest(image)? != receipt.service.image_sha256
        || sha256_file(&cursor)? != receipt.service.executable_sha256
    {
        return Err("service image identity mismatch".into());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    crate::install::copy_service_image(from, to)?;
    flush_tree(to)
}

#[cfg(target_os = "macos")]
fn flush_tree(path: &Path) -> Result<(), String> {
    fn flush_dir(path: &Path) -> Result<(), String> {
        for entry in std::fs::read_dir(path).map_err(error)? {
            let entry = entry.map_err(error)?;
            let ft = entry.file_type().map_err(error)?;
            if ft.is_dir() {
                flush_dir(&entry.path())?;
            } else if ft.is_file() {
                let (file, _) = aterm_uds::regular_file::open(&entry.path()).map_err(error)?;
                aterm_durable::durable(&file).map_err(error)?;
            } else if !ft.is_symlink() {
                return Err("special file in service image".into());
            }
        }
        aterm_durable::Flush::Durable.dir(path).map_err(error)
    }
    let meta = std::fs::symlink_metadata(path).map_err(error)?;
    if meta.is_dir() {
        flush_dir(path)?;
    } else if meta.is_file() {
        let (file, _) = aterm_uds::regular_file::open(path).map_err(error)?;
        aterm_durable::durable(&file).map_err(error)?;
    } else if !meta.file_type().is_symlink() {
        return Err("special file in service image".into());
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    let meta = std::fs::symlink_metadata(from).map_err(error)?;
    copy_tree_kind(from, to, meta.file_type(), Some(&meta))
}

#[cfg(not(target_os = "macos"))]
fn copy_tree_kind(
    from: &Path,
    to: &Path,
    kind: std::fs::FileType,
    dir_meta: Option<&std::fs::Metadata>,
) -> Result<(), String> {
    if kind.is_symlink() {
        std::os::unix::fs::symlink(std::fs::read_link(from).map_err(error)?, to).map_err(error)?;
    } else if kind.is_dir() {
        use std::os::unix::fs::OpenOptionsExt as _;
        let owned_meta;
        let meta = match dir_meta {
            Some(meta) => meta,
            None => {
                owned_meta = std::fs::symlink_metadata(from).map_err(error)?;
                &owned_meta
            }
        };
        std::fs::create_dir(to).map_err(error)?;
        for entry in std::fs::read_dir(from).map_err(error)? {
            let entry = entry.map_err(error)?;
            let ft = entry.file_type().map_err(error)?;
            copy_tree_kind(&entry.path(), &to.join(entry.file_name()), ft, None)?;
        }
        let dir_fd = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(to)
            .map_err(error)?;
        dir_fd.set_permissions(meta.permissions()).map_err(error)?;
        aterm_durable::durable(&dir_fd).map_err(error)?;
    } else if kind.is_file() {
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        let (mut source, src_meta) = aterm_uds::regular_file::open(from).map_err(error)?;
        let mut sink = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(src_meta.permissions().mode())
            .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS)
            .open(to)
            .map_err(error)?;
        std::io::copy(&mut source, &mut sink).map_err(error)?;
        sink.set_permissions(src_meta.permissions())
            .map_err(error)?;
        aterm_durable::durable(&sink).map_err(error)?;
    } else {
        return Err("special file in service image".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{Ed25519KeyPair, KeyPair};
    fn fixture(root: &Path) -> (SignedRelease, String, Vec<u8>) {
        let master = Ed25519KeyPair::from_seed_unchecked(&[21; 32]).unwrap();
        let signer = Ed25519KeyPair::from_seed_unchecked(&[22; 32]).unwrap();
        let key = |pair: &Ed25519KeyPair| {
            aterm_codec::base64::encode(pair.public_key().as_ref()).unwrap()
        };
        let roster = format!("schema=1\nroster_seq=3\nvalid_until='2099-01-01T00:00:00Z'\n[[machine]]\nid='m1'\npubkey='{}'\n", key(&signer)).into_bytes();
        let executable = if root.join("Contents/MacOS/aterm").is_file() {
            PathBuf::from("Contents/MacOS/aterm")
        } else {
            PathBuf::from("aterm")
        };
        let mut declaration = ServiceDeclaration {
            schema: 1,
            platform: "aarch64-apple-darwin".into(),
            image_sha256: image_digest(root).unwrap(),
            executable_sha256: sha256_file(&root.join(&executable)).unwrap(),
            executable,
            qualification_sha256: String::new(),
            protocol: ProtocolRange { min: 1, max: 2 },
        };
        let report = aterm_json::to_vec(&Qualification {
            schema: 1,
            status: "passed".into(),
            scope: "release-gate-and-signed-image-protocol".into(),
            build: 149,
            source_revision: "a".repeat(40),
            verified_source_revision: "d".repeat(40),
            verified_input_sha256: "e".repeat(64),
            release_check_evidence: "release gate accepted".into(),
            image_sha256: declaration.image_sha256.clone(),
            executable_sha256: declaration.executable_sha256.clone(),
            protocol: declaration.protocol.clone(),
            protocol_probe_passed: true,
            native_signature_verified: true,
            live_service_health_tested: false,
        })
        .unwrap();
        declaration.qualification_sha256 = digest(&report);
        let manifest = format!(
            "schema=1\nversion='0.149.0'\nbuild_number=149\ncommit='{}'\ndmg='aterm.dmg'\nsha256='{}'\nmachine_id='m1'\nroster_seq=3\n{}",
            "a".repeat(40),
            "b".repeat(64),
            aterm_toml::to_string(&Extension {
                service: declaration
            })
            .unwrap()
        );
        (
            SignedRelease {
                manifest_signature: signer.sign(manifest.as_bytes()).as_ref().to_vec(),
                manifest,
                roster_signature: master.sign(&roster).as_ref().to_vec(),
                roster,
            },
            key(&master),
            report,
        )
    }
    #[test]
    fn authenticated_store_rechecks_all_bytes_and_authority() {
        let tmp = aterm_tempfile::TempDir::new().unwrap();
        let input = tmp.path().join("input");
        std::fs::create_dir(&input).unwrap();
        std::fs::write(input.join("aterm"), b"signed executable").unwrap();
        std::fs::write(input.join("resource"), b"signed resource").unwrap();
        let (signed, key, report) = fixture(&input);
        let replacement = Ed25519KeyPair::from_seed_unchecked(&[23; 32]).unwrap();
        let replacement_key =
            aterm_codec::base64::encode(replacement.public_key().as_ref()).unwrap();
        assert!(authenticate(&signed, &report, 0, 3, &[&replacement_key, &key]).is_err());
        let root = tmp.path().join("store");
        ensure_private_dir(&root).unwrap();
        let store = Store { root };
        let receipt = store
            .materialize_with(&signed, &input, &report, 0, 3, &[&key])
            .unwrap();
        assert_eq!(
            store
                .verify_with(&receipt.service.image_sha256, 0, 3, &[&key])
                .unwrap(),
            receipt
        );
        assert!(
            store
                .verify_with(&receipt.service.image_sha256, 0, 4, &[&key])
                .is_err()
        );
        assert!(
            store
                .verify_with(&receipt.service.image_sha256, 9_000_000_000, 3, &[&key])
                .is_err()
        );
        assert!(
            store
                .materialize_with(&signed, &input, b"forged report", 0, 3, &[&key])
                .is_err()
        );
        let mut forged = signed.clone();
        forged.manifest.push_str("\n# forged");
        assert!(
            store
                .materialize_with(&forged, &input, &report, 0, 3, &[&key])
                .is_err()
        );
        std::fs::write(
            store
                .root
                .join(&receipt.service.image_sha256)
                .join(IMAGE_NAME)
                .join("resource"),
            b"modified",
        )
        .unwrap();
        assert!(
            store
                .verify_with(&receipt.service.image_sha256, 0, 3, &[&key])
                .is_err()
        );
    }
    #[test]
    fn shipped_bundle_aliases_survive_materialization_and_bind_their_targets() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let tmp = aterm_tempfile::TempDir::new().unwrap();
        let input = tmp.path().join("aterm.app");
        let binaries = input.join("Contents/MacOS");
        let resources = input.join("Contents/Resources");
        std::fs::create_dir_all(&binaries).unwrap();
        std::fs::create_dir_all(&resources).unwrap();
        std::fs::write(binaries.join("aterm"), b"bundle executable fixture").unwrap();
        std::fs::set_permissions(
            binaries.join("aterm"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        std::fs::write(binaries.join("alternate"), b"bundle executable fixture").unwrap();
        std::fs::write(resources.join("resource.dat"), b"bundle resources").unwrap();
        std::fs::write(
            input.join("Contents/Info.plist"),
            b"<plist><dict><key>CFBundleExecutable</key><string>aterm</string></dict></plist>",
        )
        .unwrap();
        for alias in aterm_types::argv0::ALIASES {
            symlink("aterm", binaries.join(alias)).unwrap();
        }

        let (signed, key, report) = fixture(&input);
        let root = tmp.path().join("store");
        ensure_private_dir(&root).unwrap();
        let store = Store { root };
        let receipt = store
            .materialize_with(&signed, &input, &report, 0, 3, &[&key])
            .unwrap();
        let executable = store.executable(&receipt).unwrap();
        let stored_binaries = executable.parent().unwrap();
        for alias in aterm_types::argv0::ALIASES {
            let path = stored_binaries.join(alias);
            assert_eq!(std::fs::read_link(&path).unwrap(), Path::new("aterm"));
            assert_eq!(
                path.canonicalize().unwrap(),
                executable.canonicalize().unwrap()
            );
        }
        let image = store
            .root
            .join(&receipt.service.image_sha256)
            .join(IMAGE_NAME);
        assert_eq!(image_digest(&image).unwrap(), receipt.service.image_sha256);
        assert!(
            store
                .verify_with(&receipt.service.image_sha256, 0, 3, &[&key])
                .is_ok()
        );

        let changed = stored_binaries.join(aterm_types::argv0::ALIASES[0]);
        std::fs::remove_file(&changed).unwrap();
        symlink("alternate", &changed).unwrap();
        assert_ne!(image_digest(&image).unwrap(), receipt.service.image_sha256);
        assert!(
            store
                .verify_with(&receipt.service.image_sha256, 0, 3, &[&key])
                .is_err()
        );

        std::fs::write(image.parent().unwrap().join("outside"), b"outside image").unwrap();
        std::fs::remove_file(&changed).unwrap();
        symlink("../../../outside", &changed).unwrap();
        assert!(image_digest(&image).is_err());
        assert!(
            store
                .verify_with(&receipt.service.image_sha256, 0, 3, &[&key])
                .is_err()
        );
    }
    #[test]
    fn absent_declaration_and_revoked_signer_refuse() {
        let tmp = aterm_tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("aterm"), b"exe").unwrap();
        let (mut signed, key, report) = fixture(tmp.path());
        let signer = Ed25519KeyPair::from_seed_unchecked(&[22; 32]).unwrap();
        signed.manifest = signed.manifest.split("[service]").next().unwrap().into();
        signed.manifest_signature = signer.sign(signed.manifest.as_bytes()).as_ref().to_vec();
        assert!(authenticate(&signed, &report, 0, 3, &[&key]).is_err());
        let (mut signed, _, report) = fixture(tmp.path());
        signed.roster = String::from_utf8(signed.roster)
            .unwrap()
            .replace("[[machine]]", "revoked=['m1']\n[[machine]]")
            .into_bytes();
        let master = Ed25519KeyPair::from_seed_unchecked(&[21; 32]).unwrap();
        signed.roster_signature = master.sign(&signed.roster).as_ref().to_vec();
        assert!(authenticate(&signed, &report, 0, 3, &[&key]).is_err());
    }
    #[test]
    fn new_roster_reauthorizes_old_release_and_revocation_removes_authority() {
        let tmp = aterm_tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("aterm"), b"exe").unwrap();
        let (mut signed, key, report) = fixture(tmp.path());
        let master = Ed25519KeyPair::from_seed_unchecked(&[21; 32]).unwrap();
        signed.roster = String::from_utf8(signed.roster)
            .unwrap()
            .replace("roster_seq=3", "roster_seq=4")
            .into_bytes();
        signed.roster_signature = master.sign(&signed.roster).as_ref().to_vec();
        assert!(authenticate(&signed, &report, 0, 4, &[&key]).is_ok());
        signed.roster = String::from_utf8(signed.roster)
            .unwrap()
            .replace("[[machine]]", "revoked=['m1']\n[[machine]]")
            .into_bytes();
        signed.roster_signature = master.sign(&signed.roster).as_ref().to_vec();
        assert!(authenticate(&signed, &report, 0, 4, &[&key]).is_err());
    }
    #[test]
    fn restaging_refreshes_authority_atomically_without_replacing_the_image() {
        use std::os::unix::fs::MetadataExt;
        let tmp = aterm_tempfile::TempDir::new().unwrap();
        let input = tmp.path().join("input");
        std::fs::create_dir(&input).unwrap();
        std::fs::write(input.join("aterm"), b"image bytes").unwrap();
        let (signed, key, report) = fixture(&input);
        let root = tmp.path().join("store");
        ensure_private_dir(&root).unwrap();
        let store = Store { root };
        let receipt = store
            .materialize_with(&signed, &input, &report, 0, 3, &[&key])
            .unwrap();
        let executable = store.executable(&receipt).unwrap();
        let inode = std::fs::metadata(&executable).unwrap().ino();
        assert!(
            store
                .verify_with(&receipt.service.image_sha256, 0, 4, &[&key])
                .is_err()
        );
        let mut newer = signed.clone();
        newer.roster = String::from_utf8(newer.roster)
            .unwrap()
            .replace("roster_seq=3", "roster_seq=4")
            .into_bytes();
        newer.roster_signature = Ed25519KeyPair::from_seed_unchecked(&[21; 32])
            .unwrap()
            .sign(&newer.roster)
            .as_ref()
            .to_vec();
        #[cfg(target_os = "macos")]
        {
            crate::manifest::fail_next_durable_write();
            assert!(
                store
                    .materialize_with(&newer, &input, &report, 0, 4, &[&key])
                    .is_err()
            );
            assert!(
                store
                    .verify_with(&receipt.service.image_sha256, 0, 3, &[&key])
                    .is_ok()
            );
        }
        assert_eq!(
            store
                .materialize_with(&newer, &input, &report, 0, 4, &[&key])
                .unwrap(),
            receipt
        );
        assert_eq!(
            store
                .verify_with(&receipt.service.image_sha256, 0, 4, &[&key])
                .unwrap(),
            receipt
        );
        assert_eq!(std::fs::metadata(&executable).unwrap().ino(), inode);
    }
    #[test]
    fn even_authentically_signed_failed_or_different_qualifications_refuse() {
        let tmp = aterm_tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("aterm"), b"exe").unwrap();
        let (signed, key, report) = fixture(tmp.path());
        let signer = Ed25519KeyPair::from_seed_unchecked(&[22; 32]).unwrap();
        for case in 0..5 {
            let mut failed: Qualification = aterm_json::from_slice(&report).unwrap();
            match case {
                0 => failed.status = "failed".into(),
                1 => failed.schema = 2,
                2 => failed.image_sha256 = "f".repeat(64),
                3 => failed.release_check_evidence.clear(),
                _ => failed.native_signature_verified = false,
            }
            let failed_report = aterm_json::to_vec(&failed).unwrap();
            let mut candidate = signed.clone();
            candidate.manifest = candidate
                .manifest
                .replace(&digest(&report), &digest(&failed_report));
            candidate.manifest_signature =
                signer.sign(candidate.manifest.as_bytes()).as_ref().to_vec();
            assert!(
                authenticate(&candidate, &failed_report, 0, 3, &[&key]).is_err(),
                "case {case}"
            );
        }
    }
    #[test]
    fn linked_escape_and_protocol_gaps_refuse() {
        let tmp = aterm_tempfile::TempDir::new().unwrap();
        std::os::unix::fs::symlink("/etc/passwd", tmp.path().join("escape")).unwrap();
        assert!(image_digest(tmp.path()).is_err());
        let p = ProtocolRange { min: 2, max: 4 };
        assert!(p.supports(&[2, 3, 4]));
        assert!(!p.supports(&[1, 4]));
        assert!(!p.supports(&[2, 5]));
        assert!(!ProtocolRange { min: 0, max: 4 }.supports(&[]));
        for root in [
            "relative",
            "/tmp/service-store",
            "/home/test/target/service-store",
            "/home/test/Library/Caches/service-store",
        ] {
            assert!(Store::new(root.into()).is_err());
        }
    }
    #[test]
    fn reentering_symlinks_symlinked_executables_and_open_or_unknown_receipts_refuse() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let tmp = aterm_tempfile::TempDir::new().unwrap();
        let reenter = tmp.path().join("reenter");
        std::fs::create_dir(&reenter).unwrap();
        std::fs::write(reenter.join("aterm"), b"exe").unwrap();
        // Resolves back inside `reenter`, yet steps above root via `..`.
        symlink("../reenter/aterm", reenter.join("bounce")).unwrap();
        assert!(image_digest(&reenter).is_err());

        let input = tmp.path().join("input");
        std::fs::create_dir(&input).unwrap();
        std::fs::write(input.join("aterm"), b"exe").unwrap();
        symlink("aterm", input.join("alias")).unwrap();
        let (mut signed, key, report) = fixture(&input);
        let signer = Ed25519KeyPair::from_seed_unchecked(&[22; 32]).unwrap();
        signed.manifest = signed
            .manifest
            .replace("executable = \"aterm\"", "executable = \"alias\"");
        signed.manifest_signature = signer.sign(signed.manifest.as_bytes()).as_ref().to_vec();
        let root = tmp.path().join("store");
        ensure_private_dir(&root).unwrap();
        let store = Store { root };
        assert!(
            store
                .materialize_with(&signed, &input, &report, 0, 3, &[&key])
                .is_err()
        );

        let (signed, key, report) = fixture(&input);
        let receipt = store
            .materialize_with(&signed, &input, &report, 0, 3, &[&key])
            .unwrap();
        let receipt_path = store
            .root
            .join(&receipt.service.image_sha256)
            .join("receipt.json");
        let saved = std::fs::read(&receipt_path).unwrap();
        std::fs::set_permissions(&receipt_path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            store
                .verify_with(&receipt.service.image_sha256, 0, 3, &[&key])
                .is_err()
        );
        let mut with_extra: aterm_json::Value = aterm_json::from_slice(&saved).unwrap();
        with_extra
            .as_object_mut()
            .unwrap()
            .insert("unexpected".into(), aterm_json::json!(true));
        aterm_uds::private_file::write_private(
            receipt_path.parent().unwrap(),
            "receipt.json",
            &aterm_json::to_vec(&with_extra).unwrap(),
        )
        .unwrap();
        assert!(
            store
                .verify_with(&receipt.service.image_sha256, 0, 3, &[&key])
                .is_err()
        );
    }

    /// NEGATIVE CONTROL: symlinked store ancestors are refused before directory creation, and
    /// dangling symlinks or regular files at a content-addressed store slot refuse materialization.
    #[test]
    fn symlinked_store_ancestors_and_non_directory_slots_refuse() {
        use std::os::unix::fs::symlink;

        let tmp = aterm_tempfile::TempDir::new().unwrap();
        let canonical = tmp.path().canonicalize().unwrap();
        let real_parent = canonical.join("real");
        std::fs::create_dir(&real_parent).unwrap();
        assert!(check_no_symlink_ancestors(&real_parent.join("service-images")).is_ok());

        let linked_parent = canonical.join("linked");
        symlink(&real_parent, &linked_parent).unwrap();
        assert!(check_no_symlink_ancestors(&linked_parent.join("service-images")).is_err());

        let input = canonical.join("input");
        std::fs::create_dir(&input).unwrap();
        std::fs::write(input.join("aterm"), b"exe").unwrap();
        let (signed, key, report) = fixture(&input);
        let expected_id = image_digest(&input).unwrap();

        let root = canonical.join("store");
        ensure_private_dir(&root).unwrap();
        let store = Store { root };

        let slot = store.root.join(&expected_id);
        symlink(canonical.join("nonexistent"), &slot).unwrap();
        let err = store
            .materialize_with(&signed, &input, &report, 0, 3, &[&key])
            .expect_err("dangling symlink at store slot must refuse");
        assert!(err.contains("real directory"), "{err}");
        std::fs::remove_file(&slot).unwrap();

        std::fs::write(&slot, b"not a directory").unwrap();
        let err = store
            .materialize_with(&signed, &input, &report, 0, 3, &[&key])
            .expect_err("regular file at store slot must refuse");
        assert!(err.contains("real directory"), "{err}");
    }
}
