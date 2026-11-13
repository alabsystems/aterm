// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Content-addressed copies of exact signed package and index documents.
//!
//! A logical ref is only a hint: every read re-hashes the object named by the ref. The caller
//! then re-runs the signature, parser and identity checks that made the corresponding
//! [`SignedArtifactCache::store_verified_pair`] or package/index store admissible. A partial,
//! changed or linked file is a cache miss, never authority.

#[cfg(any(test, not(unix)))]
use std::path::{Path, PathBuf};

use crate::store::Layout;

const ROOT: &str = "signed-artifacts";
const VERSION: &str = "v2";
const OBJECT_MAGIC: &[u8] = b"atpkg signed artifact object v2\0";
const INDEX_OBJECT_MAGIC: &[u8] = b"atpkg signed index object v2\0";
const REF_HEADER: &str = "atpkg signed artifact ref v2";
const KEY_DIGEST_LEN: usize = 64;
const REF_CAP: usize = 256;

/// The exact request a cached signed pair answers.
pub struct SignedArtifactKey<'a> {
    source: &'a str,
    repo: &'a str,
    program: &'a str,
    build: u64,
}

impl<'a> SignedArtifactKey<'a> {
    /// One package manifest and its detached signature, under the source and build that name it.
    pub const fn package(source: &'a str, repo: &'a str, program: &'a str, build: u64) -> Self {
        Self {
            source,
            repo,
            program,
            build,
        }
    }

    fn digest(&self) -> String {
        let mut bytes = self.identity_bytes();
        bytes.extend_from_slice(&self.build.to_be_bytes());
        crate::tree::sha256_bytes(&bytes)
    }

    fn identity_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"atpkg signed package identity v1\0");
        for field in [
            self.source.as_bytes(),
            self.repo.as_bytes(),
            self.program.as_bytes(),
        ] {
            bytes.extend_from_slice(&u64::try_from(field.len()).unwrap_or(u64::MAX).to_be_bytes());
            bytes.extend_from_slice(field);
        }
        bytes
    }
}

/// One exact release document and its detached-signature asset.
///
/// `name` names the document asset; its signature is the sibling `<name>.sig`. The key is a
/// source identity plus immutable release tag and name, so a channel pointer must never be used
/// as `tag`.
pub struct SignedReleaseKey<'a> {
    source: &'a str,
    tag: &'a str,
    name: &'a str,
}

impl<'a> SignedReleaseKey<'a> {
    /// Name an exact signed document under one immutable release tag.
    #[must_use]
    pub fn asset(source: &'a str, tag: &'a str, name: &'a str) -> Option<Self> {
        immutable_version_tag(tag)
            .then_some(())
            .filter(|()| !source.is_empty() && exact_component(name))
            .map(|()| Self { source, tag, name })
    }

    fn digest(&self) -> String {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"atpkg signed release asset identity v1\0");
        for field in [
            self.source.as_bytes(),
            self.tag.as_bytes(),
            self.name.as_bytes(),
        ] {
            bytes.extend_from_slice(&u64::try_from(field.len()).unwrap_or(u64::MAX).to_be_bytes());
            bytes.extend_from_slice(field);
        }
        crate::tree::sha256_bytes(&bytes)
    }
}

/// One exact signed index release and its authorizing roster.
pub struct SignedIndexKey<'a> {
    source: &'a str,
    label: &'a str,
}

impl<'a> SignedIndexKey<'a> {
    /// The index candidate published under `label` by `source`.
    pub fn release(source: &'a str, label: &'a str) -> Option<Self> {
        label
            .strip_prefix("atpkg-index-")
            .filter(|digits| canonical_number(digits))
            .filter(|_| !source.is_empty())
            .map(|_| Self { source, label })
    }

    fn digest(&self) -> String {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"atpkg signed index identity v1\0");
        for field in [self.source.as_bytes(), self.label.as_bytes()] {
            bytes.extend_from_slice(&u64::try_from(field.len()).unwrap_or(u64::MAX).to_be_bytes());
            bytes.extend_from_slice(field);
        }
        crate::tree::sha256_bytes(&bytes)
    }
}

/// A durable content-addressed cache rooted in one package-manager layout.
pub struct SignedArtifactCache<'a> {
    layout: &'a Layout,
}

impl<'a> SignedArtifactCache<'a> {
    /// Use `layout`'s private package prefix for immutable objects and exact logical refs.
    #[must_use]
    pub const fn for_layout(layout: &'a Layout) -> Self {
        Self { layout }
    }

    #[cfg(any(test, not(unix)))]
    fn root(&self) -> PathBuf {
        self.layout.prefix.join(ROOT).join(VERSION)
    }

    #[cfg(any(test, not(unix)))]
    fn ref_path(&self, key_digest: &str) -> Option<PathBuf> {
        let shard = key_digest.get(..2)?;
        Some(self.root().join("refs").join(shard).join(key_digest))
    }

    #[cfg(any(test, not(unix)))]
    fn object_path(&self, object_digest: &str) -> Option<PathBuf> {
        let shard = object_digest.get(..2)?;
        Some(
            self.root()
                .join("objects")
                .join("sha256")
                .join(shard)
                .join(object_digest),
        )
    }

    /// Locate the object one logical ref currently names, for corruption negative controls.
    #[cfg(test)]
    pub(crate) fn object_path_for_key(&self, key: &SignedArtifactKey<'_>) -> Option<PathBuf> {
        let key_digest = key.digest();
        let reference =
            crate::metadata_io::read_bounded_regular(&self.ref_path(&key_digest)?, REF_CAP).ok()?;
        self.object_path(reference_object_digest(&reference, &key_digest)?)
    }

    /// Locate one exact index candidate's object for corruption negative controls.
    #[cfg(test)]
    pub(crate) fn object_path_for_index(&self, key: &SignedIndexKey<'_>) -> Option<PathBuf> {
        let key_digest = key.digest();
        let reference =
            crate::metadata_io::read_bounded_regular(&self.ref_path(&key_digest)?, REF_CAP).ok()?;
        self.object_path(reference_object_digest(&reference, &key_digest)?)
    }

    /// Read a pair whose raw document and signature are bounded by the supplied caps.
    ///
    /// The returned bytes have only content-addressed integrity. Their signature and semantic
    /// identity must still be checked by the caller before use.
    #[must_use]
    pub fn load(
        &self,
        key: &SignedArtifactKey<'_>,
        document_cap: usize,
        signature_cap: usize,
    ) -> Option<(Vec<u8>, Vec<u8>)> {
        self.load_pair_at(&key.digest(), document_cap, signature_cap)
    }

    /// Read an exact release document and detached signature from the cache.
    ///
    /// This proves only local content-addressed integrity. The caller must rerun the signature,
    /// parser, exact tag/name identity and policy checks that authorize the pair before use.
    #[must_use]
    pub fn load_pair(
        &self,
        key: &SignedReleaseKey<'_>,
        document_cap: usize,
        signature_cap: usize,
    ) -> Option<(Vec<u8>, Vec<u8>)> {
        self.load_pair_at(&key.digest(), document_cap, signature_cap)
    }

    fn load_pair_at(
        &self,
        key_digest: &str,
        document_cap: usize,
        signature_cap: usize,
    ) -> Option<(Vec<u8>, Vec<u8>)> {
        let access = CacheAccess::open(self, Access::Read).ok()?;
        let reference = access.read_ref(key_digest, REF_CAP).ok()?;
        let object_digest = reference_object_digest(&reference, key_digest)?;
        let cap = OBJECT_MAGIC
            .len()
            .checked_add(KEY_DIGEST_LEN)?
            .checked_add(16)?
            .checked_add(document_cap)?
            .checked_add(signature_cap)?;
        let object = access.read_object(object_digest, cap).ok()?;
        if crate::tree::sha256_bytes(&object) != object_digest {
            return None;
        }
        decode_object(&object, key_digest, document_cap, signature_cap)
    }

    /// Persist a pair only after the caller verified its signature, parser and exact identity.
    ///
    /// The object lands before its ref, so an interrupted write leaves either the old answer or
    /// a miss. Objects are addressed by the hash checked on every read.
    pub fn store_verified(
        &self,
        key: &SignedArtifactKey<'_>,
        document: &[u8],
        signature: &[u8],
        document_cap: usize,
        signature_cap: usize,
    ) -> std::io::Result<()> {
        self.store_verified_pair_at(
            &key.digest(),
            document,
            signature,
            document_cap,
            signature_cap,
        )
    }

    /// Store an exact release pair only after the caller verified its detached signature,
    /// parser, tag/name identity and policy.
    ///
    /// The object lands before its ref, so interruption leaves either the previous exact answer
    /// or a miss. A store error never makes unverified bytes authoritative.
    pub fn store_verified_pair(
        &self,
        key: &SignedReleaseKey<'_>,
        document: &[u8],
        signature: &[u8],
        document_cap: usize,
        signature_cap: usize,
    ) -> std::io::Result<()> {
        self.store_verified_pair_at(
            &key.digest(),
            document,
            signature,
            document_cap,
            signature_cap,
        )
    }

    fn store_verified_pair_at(
        &self,
        key_digest: &str,
        document: &[u8],
        signature: &[u8],
        document_cap: usize,
        signature_cap: usize,
    ) -> std::io::Result<()> {
        if document.is_empty()
            || signature.is_empty()
            || document.len() > document_cap
            || signature.len() > signature_cap
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "signed artifact pair is empty or exceeds its byte bound",
            ));
        }
        if self
            .load_pair_at(key_digest, document_cap, signature_cap)
            .is_some_and(|(held_document, held_signature)| {
                held_document == document && held_signature == signature
            })
        {
            #[cfg(unix)]
            self.repair_shared_directories(key_digest)?;
            return Ok(());
        }
        let object = encode_object(key_digest, document, signature)?;
        let object_digest = crate::tree::sha256_bytes(&object);
        let access = CacheAccess::open(self, Access::Store)?;
        access.write_object(&object_digest, &object)?;
        let reference = format!("{REF_HEADER}\n{key_digest}\n{object_digest}\n");
        access.write_ref(key_digest, reference.as_bytes())?;
        Ok(())
    }

    /// Read an exact index, its signature, and the authorizing roster pair.
    ///
    /// Content addressing is only the local-integrity gate. The caller must rerun roster,
    /// signature, parser, freshness, succession and floor admission before using the candidate.
    #[must_use]
    pub fn load_index(
        &self,
        key: &SignedIndexKey<'_>,
        index_cap: usize,
        index_sig_cap: usize,
        roster_cap: usize,
        roster_sig_cap: usize,
    ) -> Option<crate::select::Candidate> {
        let key_digest = key.digest();
        let access = CacheAccess::open(self, Access::Read).ok()?;
        let reference = access.read_ref(&key_digest, REF_CAP).ok()?;
        let object_digest = reference_object_digest(&reference, &key_digest)?;
        let caps = [index_cap, index_sig_cap, roster_cap, roster_sig_cap];
        let cap = caps.iter().try_fold(
            INDEX_OBJECT_MAGIC
                .len()
                .checked_add(KEY_DIGEST_LEN)?
                .checked_add(32)?,
            |total, part| total.checked_add(*part),
        )?;
        let object = access.read_object(object_digest, cap).ok()?;
        if crate::tree::sha256_bytes(&object) != object_digest {
            return None;
        }
        let [index_bytes, sig, roster_bytes, roster_sig] =
            decode_index_object(&object, &key_digest, caps)?;
        Some(crate::select::Candidate {
            label: key.label.to_string(),
            index_bytes,
            sig,
            roster_bytes,
            roster_sig,
        })
    }

    /// Persist an index candidate only after the caller admitted its complete signature chain,
    /// parser, exact label/build identity, freshness, floors and succession.
    pub fn store_verified_index(
        &self,
        key: &SignedIndexKey<'_>,
        candidate: &crate::select::Candidate,
        index_cap: usize,
        index_sig_cap: usize,
        roster_cap: usize,
        roster_sig_cap: usize,
    ) -> std::io::Result<()> {
        if candidate.label != key.label {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "signed index candidate differs from its exact release label",
            ));
        }
        let parts = [
            candidate.index_bytes.as_slice(),
            candidate.sig.as_slice(),
            candidate.roster_bytes.as_slice(),
            candidate.roster_sig.as_slice(),
        ];
        let caps = [index_cap, index_sig_cap, roster_cap, roster_sig_cap];
        if parts
            .iter()
            .zip(caps)
            .any(|(part, cap)| part.is_empty() || part.len() > cap)
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "signed index candidate is empty or exceeds its byte bound",
            ));
        }
        let key_digest = key.digest();
        if self
            .load_index(key, index_cap, index_sig_cap, roster_cap, roster_sig_cap)
            .is_some_and(|held| {
                held.label == candidate.label
                    && held.index_bytes == candidate.index_bytes
                    && held.sig == candidate.sig
                    && held.roster_bytes == candidate.roster_bytes
                    && held.roster_sig == candidate.roster_sig
            })
        {
            #[cfg(unix)]
            self.repair_shared_directories(&key_digest)?;
            return Ok(());
        }
        let object = encode_index_object(&key_digest, parts)?;
        let object_digest = crate::tree::sha256_bytes(&object);
        let access = CacheAccess::open(self, Access::Store)?;
        access.write_object(&object_digest, &object)?;
        let reference = format!("{REF_HEADER}\n{key_digest}\n{object_digest}\n");
        access.write_ref(&key_digest, reference.as_bytes())?;
        Ok(())
    }

    /// An exact warm store repairs only its used shared directory chain. It reads
    /// the bounded ref again for the actual object shard, without copying or
    /// republishing payload bytes. A load never enters this write path.
    #[cfg(unix)]
    fn repair_shared_directories(&self, key_digest: &str) -> std::io::Result<()> {
        if self.layout.is_system_prefix() {
            let access = CacheAccess::open(self, Access::Repair)?;
            let reference = access.read_ref(key_digest, REF_CAP)?;
            let object_digest =
                reference_object_digest(&reference, key_digest).ok_or_else(invalid_digest)?;
            let key_shard = key_digest.get(..2).ok_or_else(invalid_digest)?;
            let object_shard = object_digest.get(..2).ok_or_else(invalid_digest)?;
            descend(
                &access.namespace,
                &["refs", key_shard],
                false,
                access.shared,
            )?;
            descend(
                &access.namespace,
                &["objects", "sha256", object_shard],
                false,
                access.shared,
            )?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum Access {
    Read,
    Store,
    #[cfg(unix)]
    Repair,
}

/// Descriptor-confined access to one cache namespace. On Unix every descendant is opened one
/// component at a time with `O_NOFOLLOW`, and publication stays relative to a held shard.
struct CacheAccess<'a> {
    #[cfg(not(unix))]
    cache: &'a SignedArtifactCache<'a>,
    #[cfg(unix)]
    namespace: std::fs::File,
    #[cfg(unix)]
    shared: bool,
    #[cfg(unix)]
    marker: std::marker::PhantomData<&'a SignedArtifactCache<'a>>,
}

impl<'a> CacheAccess<'a> {
    fn open(cache: &'a SignedArtifactCache<'a>, access: Access) -> std::io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;

            let shared = cache.layout.is_system_prefix();
            let create = matches!(access, Access::Store);
            if create {
                cache.layout.ensure_dir(&cache.layout.prefix)?;
            }
            let prefix = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(
                    libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
                )
                .open(&cache.layout.prefix)?;
            if shared && matches!(access, Access::Repair) {
                normalize_shared_directory(&prefix)?;
            }
            let namespace = descend(
                &prefix,
                &[ROOT, VERSION],
                create,
                shared && !matches!(access, Access::Read),
            )?;
            Ok(Self {
                namespace,
                shared,
                marker: std::marker::PhantomData,
            })
        }
        #[cfg(not(unix))]
        {
            let _ = access;
            Ok(Self { cache })
        }
    }

    fn read_ref(&self, digest: &str, cap: usize) -> std::io::Result<Vec<u8>> {
        self.read("refs", digest, cap)
    }

    fn read_object(&self, digest: &str, cap: usize) -> std::io::Result<Vec<u8>> {
        #[cfg(unix)]
        {
            let sha256 = descend(&self.namespace, &["objects", "sha256"], false, false)?;
            read_sharded(&sha256, digest, cap)
        }
        #[cfg(not(unix))]
        {
            let path = self.cache.object_path(digest).ok_or_else(invalid_digest)?;
            crate::metadata_io::read_bounded_regular(&path, cap)
        }
    }

    fn read(&self, _family: &str, digest: &str, cap: usize) -> std::io::Result<Vec<u8>> {
        #[cfg(unix)]
        {
            let family = descend(&self.namespace, &[_family], false, false)?;
            read_sharded(&family, digest, cap)
        }
        #[cfg(not(unix))]
        {
            let path = self.cache.ref_path(digest).ok_or_else(invalid_digest)?;
            crate::metadata_io::read_bounded_regular(&path, cap)
        }
    }

    fn write_ref(&self, digest: &str, bytes: &[u8]) -> std::io::Result<()> {
        self.write("refs", digest, bytes)
    }

    fn write_object(&self, digest: &str, bytes: &[u8]) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            let sha256 = descend(&self.namespace, &["objects", "sha256"], true, self.shared)?;
            write_sharded(&sha256, digest, bytes, self.shared)
        }
        #[cfg(not(unix))]
        {
            let object = self.cache.object_path(digest).ok_or_else(invalid_digest)?;
            let reference = self.cache.ref_path(digest).ok_or_else(invalid_digest)?;
            ensure_chain(self.cache.layout, &self.cache.root(), &object, &reference)?;
            aterm_uds::atomic_file::write(&object, bytes, 0o600)
        }
    }

    fn write(&self, _family: &str, digest: &str, bytes: &[u8]) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            let family = descend(&self.namespace, &[_family], true, self.shared)?;
            write_sharded(&family, digest, bytes, self.shared)
        }
        #[cfg(not(unix))]
        {
            let reference = self.cache.ref_path(digest).ok_or_else(invalid_digest)?;
            let object = self.cache.object_path(digest).ok_or_else(invalid_digest)?;
            ensure_chain(self.cache.layout, &self.cache.root(), &object, &reference)?;
            aterm_uds::atomic_file::write(&reference, bytes, 0o600)
        }
    }
}

fn reference_object_digest<'a>(reference: &'a [u8], key_digest: &str) -> Option<&'a str> {
    let reference = std::str::from_utf8(reference).ok()?.strip_suffix('\n')?;
    let mut lines = reference.split('\n');
    if lines.next()? != REF_HEADER || lines.next()? != key_digest {
        return None;
    }
    let object_digest = lines.next()?;
    if lines.next().is_some() || !lower_sha256(object_digest) {
        return None;
    }
    Some(object_digest)
}

#[cfg(unix)]
fn descend(
    start: &std::fs::File,
    components: &[&str],
    create: bool,
    shared: bool,
) -> std::io::Result<std::fs::File> {
    use aterm_dirfd::{Mode, OFlags};

    let flags =
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let mut current = start.try_clone()?;
    for component in components {
        if !exact_component(component) {
            return Err(invalid_digest());
        }
        let opened = || {
            aterm_uds::kept_out(|| {
                aterm_dirfd::openat(&current, component, flags, Mode::empty())
                    .map(std::fs::File::from)
                    .map_err(std::io::Error::from)
            })
        };
        current = match opened() {
            Ok(directory) => directory,
            Err(error) if create && error.kind() == std::io::ErrorKind::NotFound => {
                let mode = Mode::from_bits(if shared { 0o755 } else { 0o700 });
                match aterm_dirfd::mkdirat(&current, component, mode) {
                    Ok(()) => {}
                    Err(error) if error.raw_os_error() == libc::EEXIST => {}
                    Err(error) => return Err(error.into()),
                }
                opened()?
            }
            Err(error) => return Err(error),
        };
        if shared {
            normalize_shared_directory(&current)?;
        }
    }
    Ok(current)
}

/// Normalize only a system writer's held `O_DIRECTORY|O_NOFOLLOW` descriptor.
#[cfg(unix)]
fn normalize_shared_directory(directory: &std::fs::File) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    if directory.metadata()?.permissions().mode() & 0o7777 != 0o755 {
        aterm_dirfd::fchmod(directory, aterm_dirfd::Mode::from_bits(0o755))?;
    }
    Ok(())
}

#[cfg(unix)]
fn read_sharded(parent: &std::fs::File, digest: &str, cap: usize) -> std::io::Result<Vec<u8>> {
    use aterm_dirfd::{Mode, OFlags};

    if !lower_sha256(digest) {
        return Err(invalid_digest());
    }
    let shard = digest.get(..2).ok_or_else(invalid_digest)?;
    let directory = descend(parent, &[shard], false, false)?;
    let file = aterm_uds::kept_out(|| {
        aterm_dirfd::openat(
            &directory,
            digest,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(std::fs::File::from)
        .map_err(std::io::Error::from)
    })?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "signed artifact cache entry is not a regular file",
        ));
    }
    crate::metadata_io::read_bounded_regular_file(file, &metadata, cap)
}

#[cfg(unix)]
fn write_sharded(
    parent: &std::fs::File,
    digest: &str,
    bytes: &[u8],
    shared: bool,
) -> std::io::Result<()> {
    use aterm_dirfd::{AtFlags, Mode, OFlags};
    use std::io::Write as _;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
    if !lower_sha256(digest) {
        return Err(invalid_digest());
    }
    let shard = digest.get(..2).ok_or_else(invalid_digest)?;
    let directory = descend(parent, &[shard], true, shared)?;
    let file_mode = if shared {
        Mode::RUSR | Mode::WUSR | Mode::RGRP | Mode::ROTH
    } else {
        Mode::RUSR | Mode::WUSR
    };
    for _ in 0..8 {
        let nonce = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let temporary = format!(".{digest}.tmp.{}.{nonce}", std::process::id());
        let mut file = match aterm_uds::kept_out(|| {
            aterm_dirfd::openat(
                &directory,
                &temporary,
                OFlags::WRONLY
                    | OFlags::CREATE
                    | OFlags::EXCL
                    | OFlags::NOFOLLOW
                    | OFlags::NONBLOCK
                    | OFlags::CLOEXEC,
                file_mode,
            )
            .map(std::fs::File::from)
            .map_err(std::io::Error::from)
        }) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        let published = (|| {
            aterm_dirfd::fchmod(&file, file_mode)?;
            file.write_all(bytes)?;
            aterm_durable::Flush::Durable.file(&file)?;
            aterm_dirfd::renameat(&directory, &temporary, &directory, digest)?;
            let _ = aterm_durable::DirFlushed::classify(
                aterm_durable::Flush::Durable.file(&directory),
            )?;
            Ok::<(), std::io::Error>(())
        })();
        if published.is_err() {
            let _ = aterm_dirfd::unlinkat(&directory, &temporary, AtFlags::empty());
        }
        return published;
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "signed artifact temporary names collided",
    ))
}

#[cfg(not(unix))]
fn ensure_chain(
    layout: &Layout,
    root: &Path,
    object: &Path,
    reference: &Path,
) -> std::io::Result<()> {
    for dir in [
        root.to_path_buf(),
        root.join("objects"),
        root.join("objects").join("sha256"),
        object
            .parent()
            .ok_or_else(|| std::io::Error::other("content object has no parent"))?
            .to_path_buf(),
        root.join("refs"),
        reference
            .parent()
            .ok_or_else(|| std::io::Error::other("artifact ref has no parent"))?
            .to_path_buf(),
    ] {
        layout.ensure_dir(&dir)?;
    }
    Ok(())
}

fn invalid_digest() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "signed artifact digest is not addressable",
    )
}

fn lower_sha256(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn canonical_number(digits: &str) -> bool {
    !digits.is_empty()
        && digits.bytes().all(|byte| byte.is_ascii_digit())
        && (digits == "0" || !digits.starts_with('0'))
}

fn immutable_version_tag(tag: &str) -> bool {
    let Some(version) = tag.strip_prefix('v') else {
        return false;
    };
    let mut fields = version.split('.');
    matches!(
        (fields.next(), fields.next(), fields.next(), fields.next()),
        (Some(major), Some(minor), Some(patch), None)
            if canonical_number(major) && canonical_number(minor) && canonical_number(patch)
    )
}

fn exact_component(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.bytes().any(|byte| matches!(byte, 0 | b'/' | b'\\'))
}

fn encode_object(key_digest: &str, document: &[u8], signature: &[u8]) -> std::io::Result<Vec<u8>> {
    if !lower_sha256(key_digest) {
        return Err(invalid_digest());
    }
    let document_len = u64::try_from(document.len()).map_err(|_| invalid_digest())?;
    let signature_len = u64::try_from(signature.len()).map_err(|_| invalid_digest())?;
    let capacity = OBJECT_MAGIC
        .len()
        .checked_add(KEY_DIGEST_LEN)
        .and_then(|n| n.checked_add(16))
        .and_then(|n| n.checked_add(document.len()))
        .and_then(|n| n.checked_add(signature.len()))
        .ok_or_else(invalid_digest)?;
    let mut object = Vec::new();
    object.try_reserve_exact(capacity).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::OutOfMemory,
            format!("signed artifact allocation refused: {error}"),
        )
    })?;
    object.extend_from_slice(OBJECT_MAGIC);
    object.extend_from_slice(key_digest.as_bytes());
    object.extend_from_slice(&document_len.to_be_bytes());
    object.extend_from_slice(&signature_len.to_be_bytes());
    object.extend_from_slice(document);
    object.extend_from_slice(signature);
    Ok(object)
}

fn decode_object(
    object: &[u8],
    key_digest: &str,
    document_cap: usize,
    signature_cap: usize,
) -> Option<(Vec<u8>, Vec<u8>)> {
    if object.get(..OBJECT_MAGIC.len())? != OBJECT_MAGIC {
        return None;
    }
    let key_end = OBJECT_MAGIC.len().checked_add(KEY_DIGEST_LEN)?;
    if object.get(OBJECT_MAGIC.len()..key_end)? != key_digest.as_bytes() {
        return None;
    }
    let lengths_end = key_end.checked_add(16)?;
    let lengths = object.get(key_end..lengths_end)?;
    let document_len =
        usize::try_from(u64::from_be_bytes(lengths.get(..8)?.try_into().ok()?)).ok()?;
    let signature_len =
        usize::try_from(u64::from_be_bytes(lengths.get(8..)?.try_into().ok()?)).ok()?;
    if document_len == 0
        || signature_len == 0
        || document_len > document_cap
        || signature_len > signature_cap
    {
        return None;
    }
    let start = lengths_end;
    let split = start.checked_add(document_len)?;
    let end = split.checked_add(signature_len)?;
    if end != object.len() {
        return None;
    }
    Some((
        object.get(start..split)?.to_vec(),
        object.get(split..end)?.to_vec(),
    ))
}

fn encode_index_object(key_digest: &str, parts: [&[u8]; 4]) -> std::io::Result<Vec<u8>> {
    if !lower_sha256(key_digest) {
        return Err(invalid_digest());
    }
    let capacity = parts.iter().try_fold(
        INDEX_OBJECT_MAGIC
            .len()
            .checked_add(KEY_DIGEST_LEN)
            .and_then(|n| n.checked_add(32))
            .ok_or_else(invalid_digest)?,
        |total, part| total.checked_add(part.len()).ok_or_else(invalid_digest),
    )?;
    let mut object = Vec::new();
    object.try_reserve_exact(capacity).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::OutOfMemory,
            format!("signed index allocation refused: {error}"),
        )
    })?;
    object.extend_from_slice(INDEX_OBJECT_MAGIC);
    object.extend_from_slice(key_digest.as_bytes());
    for length in parts.map(|part| u64::try_from(part.len()).map_err(|_| invalid_digest())) {
        let length = length?;
        object.extend_from_slice(&length.to_be_bytes());
    }
    for part in parts {
        object.extend_from_slice(part);
    }
    Ok(object)
}

fn decode_index_object(object: &[u8], key_digest: &str, caps: [usize; 4]) -> Option<[Vec<u8>; 4]> {
    if object.get(..INDEX_OBJECT_MAGIC.len())? != INDEX_OBJECT_MAGIC {
        return None;
    }
    let key_end = INDEX_OBJECT_MAGIC.len().checked_add(KEY_DIGEST_LEN)?;
    if object.get(INDEX_OBJECT_MAGIC.len()..key_end)? != key_digest.as_bytes() {
        return None;
    }
    let lengths_start = key_end;
    let lengths_end = lengths_start.checked_add(32)?;
    let lengths = object.get(lengths_start..lengths_end)?;
    let mut decoded = [0usize; 4];
    for (slot, chunk) in lengths.as_chunks::<8>().0.iter().enumerate() {
        decoded[slot] = usize::try_from(u64::from_be_bytes(*chunk)).ok()?;
    }
    if decoded
        .iter()
        .zip(caps)
        .any(|(length, cap)| *length == 0 || *length > cap)
    {
        return None;
    }
    let mut at = lengths_end;
    let mut parts: [Vec<u8>; 4] = Default::default();
    for (slot, length) in decoded.into_iter().enumerate() {
        let end = at.checked_add(length)?;
        parts[slot] = object.get(at..end)?.to_vec();
        at = end;
    }
    (at == object.len()).then_some(parts)
}

#[cfg(test)]
#[path = "signed_artifact_cache_tests.rs"]
mod tests;
