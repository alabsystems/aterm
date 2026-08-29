// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fail-closed integrity checks for aterm's managed Ollama model store.
//!
//! The model is terminal-context code in the privacy sense: substituted weights can
//! exfiltrate or reproduce prompts even when the runtime executable is authentic.
//! Managed launch therefore accepts only a sealed tree and hashes every blob named by
//! the selected manifest. This expensive check runs once per owned daemon authority.
//! Routine title refreshes revalidate the anchored path/inode/ctime/mtime/size/mode
//! snapshot instead of streaming multi-gigabyte weights again.
//!
//! Threat boundary: this detects same-UID on-disk substitution (including a writer
//! that restores mtime and permissions) and the caller warms/pins the verified model
//! before sending terminal context. It does not claim to defeat root, kernel
//! compromise, or a process that already has permission to modify the authenticated
//! daemon's address space; runtime code-signing and platform process protections own
//! that separate boundary.

#[cfg(target_os = "macos")]
use aterm_digest::Sha256;
#[cfg(target_os = "macos")]
use std::collections::HashSet;
#[cfg(target_os = "macos")]
use std::io::Read as _;
#[cfg(target_os = "macos")]
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
#[cfg(target_os = "macos")]
use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
const MAX_MODEL_TREE_ENTRIES: usize = 4_096;
#[cfg(target_os = "macos")]
const MAX_MODEL_NAME_BYTES: usize = 256;
#[cfg(target_os = "macos")]
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
#[cfg(target_os = "macos")]
const MAX_MANIFEST_BLOBS: usize = 64;
#[cfg(target_os = "macos")]
const MAX_BLOB_BYTES: u64 = 16 * 1024 * 1024 * 1024;
#[cfg(target_os = "macos")]
const MAX_MANIFEST_TOTAL_BYTES: u64 = 32 * 1024 * 1024 * 1024;

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PinnedDescriptor<'a> {
    media_type: &'a str,
    digest: &'a str,
    size: u64,
}

/// Supply-chain anchor for the one model aterm downloaded and offers as its managed
/// default. Manifest-to-blob hashing alone proves only self-consistency: without this
/// embedded descriptor set an attacker could replace both and recompute the names.
#[cfg(target_os = "macos")]
const QWEN_35_4B_Q4_K_M_DESCRIPTORS: &[PinnedDescriptor<'static>] = &[
    PinnedDescriptor {
        media_type: "application/vnd.docker.container.image.v1+json",
        digest: "de9fed2251b37295b763727a59ca35cf5cfe5c7379bc3e2104b2ce3c145aa887",
        size: 475,
    },
    PinnedDescriptor {
        media_type: "application/vnd.ollama.image.model",
        digest: "81fb60c7daa80fc1123380b98970b320ae233409f0f71a72ed7b9b0d62f40490",
        size: 3_389_971_840,
    },
    PinnedDescriptor {
        media_type: "application/vnd.ollama.image.license",
        digest: "7339fa418c9ad3e8e12e74ad0fd26a9cc4be8703f9c110728a992b193be85cb2",
        size: 11_355,
    },
    PinnedDescriptor {
        media_type: "application/vnd.ollama.image.params",
        digest: "9371364b27a52acac9d87f88bd93c9db1174d8d6ec57f6888925cdc1788871ff",
        size: 65,
    },
];

/// Keeps the exact manifest and referenced blob inodes open for the lifetime of the
/// owned model authority. Directory immutability protects their path bindings against
/// ordinary writes; the open handles additionally prevent accidental reclamation.
#[derive(Debug)]
pub(super) struct AttestedManagedModel {
    #[cfg(target_os = "macos")]
    _guards: Vec<std::fs::File>,
    #[cfg(target_os = "macos")]
    root: PathBuf,
    #[cfg(target_os = "macos")]
    identities: Vec<TreeIdentity>,
}

#[cfg(target_os = "macos")]
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct TreeIdentity {
    path: PathBuf,
    device: u64,
    inode: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
    owner: u32,
    mode: u32,
    links: u64,
    directory: bool,
}

#[cfg(target_os = "macos")]
fn tree_identity(path: PathBuf, metadata: &std::fs::Metadata) -> TreeIdentity {
    TreeIdentity {
        path,
        device: metadata.dev(),
        inode: metadata.ino(),
        length: metadata.len(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
        owner: metadata.uid(),
        mode: metadata.mode(),
        links: metadata.nlink(),
        directory: metadata.file_type().is_dir(),
    }
}

impl AttestedManagedModel {
    /// Cheap request-boundary check over the exact tree that passed the one-time
    /// pinned hash. ctime is intentionally part of the identity: a same-UID writer
    /// can restore contents, length, permissions, and mtime, but cannot set ctime.
    #[cfg(target_os = "macos")]
    pub(super) fn revalidate(&self) -> Result<(), String> {
        let current = inspect_sealed_tree(&self.root)?;
        if current != self.identities {
            return Err("managed model tree changed after integrity verification".to_string());
        }
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    pub(super) fn revalidate(&self) -> Result<(), String> {
        Err("managed model integrity revalidation is unavailable on this platform".to_string())
    }
}

#[cfg(target_os = "macos")]
fn validate_tree_entry(
    root: &Path,
    path: &Path,
    metadata: &std::fs::Metadata,
    uid: u32,
) -> Result<(), String> {
    if !path.starts_with(root) {
        return Err("managed model path escapes its model root".to_string());
    }
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "managed model tree contains a link at {}",
            path.display()
        ));
    }
    if !metadata.file_type().is_dir() && !metadata.file_type().is_file() {
        return Err(format!(
            "managed model tree contains an unsupported entry at {}",
            path.display()
        ));
    }
    if metadata.uid() != uid {
        return Err(format!(
            "managed model entry is not owned by the current user: {}",
            path.display()
        ));
    }
    if metadata.mode() & 0o222 != 0 {
        return Err(format!(
            "managed model tree must be sealed read-only before launch: {}",
            path.display()
        ));
    }
    if metadata.file_type().is_dir() && metadata.mode() & 0o100 == 0 {
        return Err(format!(
            "managed model directory is not owner-searchable: {}",
            path.display()
        ));
    }
    if metadata.file_type().is_file() {
        if metadata.mode() & 0o6000 != 0 {
            return Err(format!(
                "managed model file must not be set-id: {}",
                path.display()
            ));
        }
        if metadata.nlink() != 1 {
            return Err(format!(
                "managed model file must have exactly one link: {}",
                path.display()
            ));
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn inspect_sealed_tree(root: &Path) -> Result<Vec<TreeIdentity>, String> {
    let uid = {
        // SAFETY: getuid has no arguments and cannot fail.
        unsafe { libc::getuid() }
    };
    let root_metadata = std::fs::symlink_metadata(root)
        .map_err(|error| format!("managed model root {}: {error}", root.display()))?;
    validate_tree_entry(root, root, &root_metadata, uid)?;
    if !root_metadata.file_type().is_dir() {
        return Err("managed model root must be a real directory".to_string());
    }
    let mut identities = vec![tree_identity(root.to_path_buf(), &root_metadata)];
    let mut pending = vec![root.to_path_buf()];
    let mut entries = 0usize;
    while let Some(directory) = pending.pop() {
        let children = std::fs::read_dir(&directory)
            .map_err(|error| format!("managed model directory {}: {error}", directory.display()))?;
        for child in children {
            let child = child.map_err(|error| format!("managed model tree entry: {error}"))?;
            entries = entries.saturating_add(1);
            if entries > MAX_MODEL_TREE_ENTRIES {
                return Err("managed model tree exceeds 4096 entries".to_string());
            }
            let path = child.path();
            let metadata = std::fs::symlink_metadata(&path)
                .map_err(|error| format!("managed model tree entry {}: {error}", path.display()))?;
            validate_tree_entry(root, &path, &metadata, uid)?;
            if metadata.file_type().is_dir() {
                pending.push(path.clone());
            }
            identities.push(tree_identity(path, &metadata));
        }
    }
    identities.sort();
    Ok(identities)
}

#[cfg(target_os = "macos")]
fn valid_model_component(component: &str) -> bool {
    !component.is_empty()
        && component.len() <= 128
        && component != "."
        && component != ".."
        && component
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// Resolve Ollama's normalized manifest layout without accepting absolute paths,
/// traversal, registry ports, percent encoding, or platform separators.
#[cfg(target_os = "macos")]
fn manifest_relative_path(model: &str) -> Result<PathBuf, String> {
    let model = model.trim();
    if model.is_empty() || model.len() > MAX_MODEL_NAME_BYTES || !model.is_ascii() {
        return Err("managed Ollama model name is empty, non-ASCII, or too long".to_string());
    }
    let (name, tag) = match model.rsplit_once(':') {
        Some((name, tag)) if !name.contains(':') => (name, tag),
        Some(_) => return Err("managed Ollama model registry ports are not supported".to_string()),
        None => (model, "latest"),
    };
    let mut components: Vec<&str> = name.split('/').collect();
    if !valid_model_component(tag)
        || components
            .iter()
            .any(|component| !valid_model_component(component))
    {
        return Err("managed Ollama model name contains an unsafe path component".to_string());
    }
    match components.len() {
        1 => {
            components.splice(0..0, ["registry.ollama.ai", "library"]);
        }
        2 => {
            components.insert(0, "registry.ollama.ai");
        }
        _ => {}
    }
    let mut relative = PathBuf::from("manifests");
    for component in components {
        relative.push(component);
    }
    relative.push(tag);
    Ok(relative)
}

#[cfg(target_os = "macos")]
fn open_sealed_file(path: &Path, uid: u32, limit: u64) -> Result<std::fs::File, String> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| format!("managed model file {}: {error}", path.display()))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("managed model metadata {}: {error}", path.display()))?;
    if !metadata.file_type().is_file()
        || metadata.uid() != uid
        || metadata.mode() & 0o222 != 0
        || metadata.mode() & 0o6000 != 0
        || metadata.nlink() != 1
        || metadata.len() > limit
    {
        return Err(format!(
            "managed model file has unsafe type, ownership, permissions, links, or size: {}",
            path.display()
        ));
    }
    Ok(file)
}

#[cfg(target_os = "macos")]
fn digest_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

#[cfg(target_os = "macos")]
fn descriptor_digest_and_size(
    descriptor: &aterm_json::Value,
) -> Result<(String, String, u64), String> {
    let media_type = descriptor
        .get("mediaType")
        .and_then(aterm_json::Value::as_str)
        .filter(|media_type| !media_type.is_empty() && media_type.len() <= 256)
        .ok_or_else(|| "managed model manifest descriptor lacks a media type".to_string())?;
    let digest = descriptor
        .get("digest")
        .and_then(aterm_json::Value::as_str)
        .and_then(|digest| digest.strip_prefix("sha256:"))
        .ok_or_else(|| "managed model manifest descriptor lacks a SHA-256 digest".to_string())?;
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err("managed model manifest contains an invalid SHA-256 digest".to_string());
    }
    let size = descriptor
        .get("size")
        .and_then(aterm_json::Value::as_u64)
        .ok_or_else(|| "managed model manifest descriptor lacks a byte size".to_string())?;
    if size == 0 || size > MAX_BLOB_BYTES {
        return Err("managed model manifest blob size is outside the bounded range".to_string());
    }
    Ok((media_type.to_string(), digest.to_string(), size))
}

#[cfg(target_os = "macos")]
pub(super) fn attest_managed_model(
    models: &Path,
    model: &str,
) -> Result<AttestedManagedModel, String> {
    let pinned = match model {
        "qwen3.5:4b-q4_K_M" => QWEN_35_4B_Q4_K_M_DESCRIPTORS,
        _ => {
            return Err(
                "managed Ollama auto-launch accepts only the pinned qwen3.5:4b-q4_K_M artifact; use an explicitly trusted external provider for custom models"
                    .to_string(),
            );
        }
    };
    attest_managed_model_with_pin(models, model, pinned)
}

#[cfg(target_os = "macos")]
fn attest_managed_model_with_pin(
    models: &Path,
    model: &str,
    pinned: &[PinnedDescriptor<'_>],
) -> Result<AttestedManagedModel, String> {
    let model_link = std::fs::symlink_metadata(models)
        .map_err(|error| format!("managed model root {}: {error}", models.display()))?;
    if !model_link.file_type().is_dir() {
        return Err("managed model root must be a real directory, not a link".to_string());
    }
    let canonical_models = models
        .canonicalize()
        .map_err(|error| format!("managed model root {}: {error}", models.display()))?;
    let before = inspect_sealed_tree(&canonical_models)?;
    let manifest = canonical_models.join(manifest_relative_path(model)?);
    if !manifest.starts_with(&canonical_models) {
        return Err("managed model manifest escapes its model root".to_string());
    }
    let uid = {
        // SAFETY: getuid has no arguments and cannot fail.
        unsafe { libc::getuid() }
    };
    let mut manifest_guard = open_sealed_file(&manifest, uid, MAX_MANIFEST_BYTES)?;
    let manifest_size = manifest_guard
        .metadata()
        .map_err(|error| format!("managed model manifest metadata: {error}"))?
        .len();
    let mut manifest_bytes = Vec::with_capacity(usize::try_from(manifest_size).unwrap_or(0));
    manifest_guard
        .by_ref()
        .take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut manifest_bytes)
        .map_err(|error| format!("could not read managed model manifest: {error}"))?;
    if manifest_bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err("managed model manifest exceeds 1 MiB".to_string());
    }
    let manifest_json: aterm_json::Value = aterm_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("invalid managed model manifest JSON: {error}"))?;
    if manifest_json
        .get("schemaVersion")
        .and_then(aterm_json::Value::as_u64)
        != Some(2)
    {
        return Err("managed model manifest schemaVersion must be 2".to_string());
    }
    let config = manifest_json
        .get("config")
        .ok_or_else(|| "managed model manifest lacks its config descriptor".to_string())?;
    let layers = manifest_json
        .get("layers")
        .and_then(aterm_json::Value::as_array)
        .ok_or_else(|| "managed model manifest lacks its layer list".to_string())?;
    if layers.len().saturating_add(1) > MAX_MANIFEST_BLOBS {
        return Err("managed model manifest references more than 64 blobs".to_string());
    }

    let mut descriptors = Vec::with_capacity(layers.len().saturating_add(1));
    descriptors.push(config);
    descriptors.extend(layers);
    let mut seen = HashSet::new();
    let mut total = 0u64;
    let mut expected = Vec::with_capacity(descriptors.len());
    for descriptor in descriptors {
        let (media_type, digest, size) = descriptor_digest_and_size(descriptor)?;
        if !seen.insert(digest.clone()) {
            return Err("managed model manifest repeats a blob digest".to_string());
        }
        total = total
            .checked_add(size)
            .ok_or_else(|| "managed model manifest total size overflowed".to_string())?;
        if total > MAX_MANIFEST_TOTAL_BYTES {
            return Err("managed model manifest exceeds the 32 GiB total bound".to_string());
        }
        expected.push((media_type, digest, size));
    }
    if expected.len() != pinned.len()
        || expected.iter().zip(pinned).any(|(actual, pinned)| {
            actual.0 != pinned.media_type || actual.1 != pinned.digest || actual.2 != pinned.size
        })
    {
        return Err(
            "managed model manifest does not match aterm's pinned descriptor set".to_string(),
        );
    }

    let mut guards = Vec::with_capacity(expected.len().saturating_add(1));
    guards.push(manifest_guard);
    let mut buffer = vec![0u8; 1024 * 1024];
    for (_, digest, expected_size) in expected {
        let blob = canonical_models
            .join("blobs")
            .join(format!("sha256-{digest}"));
        if !blob.starts_with(&canonical_models) {
            return Err("managed model blob escapes its model root".to_string());
        }
        let mut guard = open_sealed_file(&blob, uid, MAX_BLOB_BYTES)?;
        let actual_size = guard
            .metadata()
            .map_err(|error| format!("managed model blob metadata: {error}"))?
            .len();
        if actual_size != expected_size {
            return Err(format!(
                "managed model blob size does not match its manifest: {}",
                blob.display()
            ));
        }
        let mut hasher = Sha256::new();
        let mut read_total = 0u64;
        loop {
            let read = guard
                .read(&mut buffer)
                .map_err(|error| format!("could not hash managed model blob: {error}"))?;
            if read == 0 {
                break;
            }
            read_total = read_total.saturating_add(read as u64);
            if read_total > expected_size {
                return Err("managed model blob grew while hashing".to_string());
            }
            hasher.update(&buffer[..read]);
        }
        if read_total != expected_size || digest_hex(&hasher.finalize()) != digest {
            return Err(format!(
                "managed model blob SHA-256 does not match its manifest: {}",
                blob.display()
            ));
        }
        guards.push(guard);
    }

    let after = inspect_sealed_tree(&canonical_models)?;
    if before != after {
        return Err("managed model tree changed during integrity verification".to_string());
    }
    Ok(AttestedManagedModel {
        _guards: guards,
        root: canonical_models,
        identities: after,
    })
}

#[cfg(not(target_os = "macos"))]
pub(super) fn attest_managed_model(
    _models: &std::path::Path,
    _model: &str,
) -> Result<AttestedManagedModel, String> {
    Err("managed model integrity attestation is unavailable on this platform".to_string())
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TREE: AtomicU64 = AtomicU64::new(1);

    struct TestTree {
        root: PathBuf,
        blob: PathBuf,
        pin: Vec<PinnedDescriptor<'static>>,
    }

    impl TestTree {
        fn new(blob_bytes: &[u8]) -> Self {
            let root = std::env::temp_dir().join(format!(
                "aterm-managed-model-test-{}-{}",
                std::process::id(),
                NEXT_TREE.fetch_add(1, Ordering::Relaxed)
            ));
            let manifest = root.join("manifests/registry.ollama.ai/library/tiny/unit");
            std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
            std::fs::create_dir_all(root.join("blobs")).unwrap();
            let digest = digest_hex(&Sha256::digest(blob_bytes));
            let blob = root.join("blobs").join(format!("sha256-{digest}"));
            std::fs::write(&blob, blob_bytes).unwrap();
            let manifest_json = aterm_json::json!({
                "schemaVersion": 2,
                "config": {
                    "mediaType": "application/vnd.ollama.image.test",
                    "digest": format!("sha256:{digest}"),
                    "size": blob_bytes.len()
                },
                "layers": []
            });
            std::fs::write(manifest, aterm_json::to_vec(&manifest_json).unwrap()).unwrap();
            seal_tree(&root);
            let leaked_digest: &'static str = Box::leak(digest.into_boxed_str());
            Self {
                root,
                blob,
                pin: vec![PinnedDescriptor {
                    media_type: "application/vnd.ollama.image.test",
                    digest: leaked_digest,
                    size: blob_bytes.len() as u64,
                }],
            }
        }
    }

    impl Drop for TestTree {
        fn drop(&mut self) {
            make_tree_writable(&self.root);
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn visit_tree(root: &Path, visit: &mut impl FnMut(&Path, &std::fs::Metadata)) {
        let metadata = std::fs::symlink_metadata(root).unwrap();
        visit(root, &metadata);
        if metadata.file_type().is_dir() {
            for entry in std::fs::read_dir(root).unwrap() {
                visit_tree(&entry.unwrap().path(), visit);
            }
        }
    }

    fn seal_tree(root: &Path) {
        let mut paths = Vec::new();
        visit_tree(root, &mut |path, metadata| {
            paths.push((path.to_path_buf(), metadata.file_type()))
        });
        paths.sort_by_key(|(path, _)| std::cmp::Reverse(path.components().count()));
        for (path, kind) in paths {
            if kind.is_symlink() {
                continue;
            }
            let mode = if kind.is_dir() { 0o555 } else { 0o444 };
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        }
    }

    fn make_tree_writable(root: &Path) {
        let Ok(metadata) = std::fs::symlink_metadata(root) else {
            return;
        };
        if metadata.file_type().is_symlink() {
            return;
        }
        if metadata.file_type().is_dir() {
            let _ = std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o755));
            if let Ok(entries) = std::fs::read_dir(root) {
                for entry in entries.flatten() {
                    make_tree_writable(&entry.path());
                }
            }
        } else {
            let _ = std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o644));
        }
    }

    #[test]
    fn sealed_manifest_and_blob_are_verified_and_held() {
        let tree = TestTree::new(b"small deterministic model blob");
        let attested = attest_managed_model_with_pin(&tree.root, "tiny:unit", &tree.pin).unwrap();
        assert_eq!(attested._guards.len(), 2);
        attested.revalidate().unwrap();
        assert_eq!(
            manifest_relative_path("tiny:unit").unwrap(),
            PathBuf::from("manifests/registry.ollama.ai/library/tiny/unit")
        );
    }

    #[test]
    fn writable_or_substituted_model_store_fails_closed() {
        let writable = TestTree::new(b"model");
        std::fs::set_permissions(&writable.blob, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(attest_managed_model_with_pin(&writable.root, "tiny:unit", &writable.pin).is_err());

        let changed = TestTree::new(b"model");
        std::fs::set_permissions(&changed.blob, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::write(&changed.blob, b"evil!").unwrap();
        std::fs::set_permissions(&changed.blob, std::fs::Permissions::from_mode(0o444)).unwrap();
        let error =
            attest_managed_model_with_pin(&changed.root, "tiny:unit", &changed.pin).unwrap_err();
        assert!(error.contains("SHA-256"));
    }

    #[test]
    fn coherent_manifest_and_blob_replacement_cannot_replace_the_pin() {
        let trusted = TestTree::new(b"trusted model");
        let replacement = TestTree::new(b"coherent malicious replacement");
        let error = attest_managed_model_with_pin(&replacement.root, "tiny:unit", &trusted.pin)
            .unwrap_err();
        assert!(error.contains("pinned descriptor set"));
        assert!(attest_managed_model(&replacement.root, "tiny:unit").is_err());
    }

    #[test]
    fn post_attestation_same_uid_mutation_invalidates_the_anchor() {
        let tree = TestTree::new(b"trusted model");
        let attested = attest_managed_model_with_pin(&tree.root, "tiny:unit", &tree.pin).unwrap();
        std::fs::set_permissions(&tree.blob, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::write(&tree.blob, b"evil replacement").unwrap();
        std::fs::set_permissions(&tree.blob, std::fs::Permissions::from_mode(0o444)).unwrap();
        let error = attested.revalidate().unwrap_err();
        assert!(error.contains("changed after integrity verification"));
    }

    #[test]
    fn model_names_and_tree_links_cannot_escape() {
        assert!(manifest_relative_path("../../secret:latest").is_err());
        assert!(manifest_relative_path("registry:443/model:latest").is_err());
        let tree = TestTree::new(b"model");
        let link = tree.root.join("unexpected-link");
        std::fs::set_permissions(&tree.root, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink(&tree.blob, &link).unwrap();
        std::fs::set_permissions(&tree.root, std::fs::Permissions::from_mode(0o555)).unwrap();
        assert!(attest_managed_model_with_pin(&tree.root, "tiny:unit", &tree.pin).is_err());
    }
}
