// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

const DOCUMENT_CAP: usize = 1024;
const SIGNATURE_CAP: usize = 128;
const ROSTER_CAP: usize = 2048;

fn fixture(label: &str) -> (crate::testdir::Dir, Layout) {
    let dir = crate::testdir::dir(&format!("atpkg-signed-artifact-{label}-"));
    let layout = Layout {
        prefix: dir.join("pkg"),
    };
    (dir, layout)
}

fn key() -> SignedArtifactKey<'static> {
    SignedArtifactKey::package("github:owner/index", "owner/repo", "gitleaks", 3)
}

fn paths(cache: &SignedArtifactCache<'_>) -> (PathBuf, PathBuf) {
    let reference = cache.ref_path(&key().digest()).unwrap();
    let object = cache.object_path_for_key(&key()).unwrap();
    (reference, object)
}

fn index_candidate(label: &str, suffix: u8) -> crate::select::Candidate {
    crate::select::Candidate {
        label: label.to_string(),
        index_bytes: vec![b'i', suffix],
        sig: vec![b's', suffix],
        roster_bytes: vec![b'r', suffix],
        roster_sig: vec![b'a', suffix],
    }
}

fn candidate_parts(candidate: &crate::select::Candidate) -> [&[u8]; 4] {
    [
        &candidate.index_bytes,
        &candidate.sig,
        &candidate.roster_bytes,
        &candidate.roster_sig,
    ]
}

fn object_digest_from_ref(reference: &Path) -> String {
    std::fs::read_to_string(reference)
        .unwrap()
        .lines()
        .nth(2)
        .unwrap()
        .to_string()
}

#[test]
fn mutable_or_noncanonical_release_names_have_no_cache_key() {
    assert!(SignedReleaseKey::asset("github:owner/repo", "latest", "appcast.toml").is_none());
    assert!(SignedReleaseKey::asset("github:owner/repo", "v01.2.3", "appcast.toml").is_none());
    assert!(SignedIndexKey::release("github:owner/index", "stable").is_none());
    assert!(SignedIndexKey::release("github:owner/index", "atpkg-index-03").is_none());
    assert!(SignedReleaseKey::asset("github:owner/repo", "v1.2.3", "appcast.toml").is_some());
    assert!(SignedIndexKey::release("github:owner/index", "atpkg-index-3").is_some());
}

#[test]
fn exact_build_refs_coexist() {
    let (_dir, layout) = fixture("exact-builds");
    let cache = SignedArtifactCache::for_layout(&layout);
    let old = key();
    cache
        .store_verified(
            &old,
            b"old signed manifest",
            b"old detached signature",
            DOCUMENT_CAP,
            SIGNATURE_CAP,
        )
        .unwrap();
    let new = SignedArtifactKey::package("github:owner/index", "owner/repo", "gitleaks", 4);
    cache
        .store_verified(
            &new,
            b"new signed manifest",
            b"new detached signature",
            DOCUMENT_CAP,
            SIGNATURE_CAP,
        )
        .unwrap();
    assert_eq!(
        cache.load(&old, DOCUMENT_CAP, SIGNATURE_CAP),
        Some((
            b"old signed manifest".to_vec(),
            b"old detached signature".to_vec()
        ))
    );
    assert_eq!(
        cache.load(&new, DOCUMENT_CAP, SIGNATURE_CAP),
        Some((
            b"new signed manifest".to_vec(),
            b"new detached signature".to_vec()
        ))
    );
    assert_ne!(
        cache.ref_path(&old.digest()),
        cache.ref_path(&new.digest()),
        "each exact signed build retains its own ref"
    );
}

#[test]
fn an_intact_pair_object_cannot_be_relinked_under_another_exact_key() {
    let (_dir, layout) = fixture("pair-key-binding");
    let cache = SignedArtifactCache::for_layout(&layout);
    let old = key();
    let new = SignedArtifactKey::package("github:owner/index", "owner/repo", "gitleaks", 4);
    for (key, document) in [(&old, b"old document".as_slice()), (&new, b"new document")] {
        cache
            .store_verified(key, document, b"signature", DOCUMENT_CAP, SIGNATURE_CAP)
            .unwrap();
    }
    let new_ref = cache.ref_path(&new.digest()).unwrap();
    let new_object = object_digest_from_ref(&new_ref);
    let old_ref = cache.ref_path(&old.digest()).unwrap();
    std::fs::write(
        old_ref,
        format!("{REF_HEADER}\n{}\n{new_object}\n", old.digest()),
    )
    .unwrap();
    assert!(
        cache.load(&old, DOCUMENT_CAP, SIGNATURE_CAP).is_none(),
        "the object envelope, not only its ref, is bound to the logical key"
    );
}

#[test]
fn exact_release_asset_pairs_use_the_public_cache_contract() {
    let (_dir, layout) = fixture("release-pairs");
    let cache = SignedArtifactCache::for_layout(&layout);
    let old = SignedReleaseKey::asset("github:alabsystems/aterm", "v0.174.0", "aterm-appcast.toml")
        .unwrap();
    let new = SignedReleaseKey::asset("github:alabsystems/aterm", "v0.175.0", "aterm-appcast.toml")
        .unwrap();
    cache
        .store_verified_pair(
            &old,
            b"old appcast",
            b"old signature",
            DOCUMENT_CAP,
            SIGNATURE_CAP,
        )
        .unwrap();
    cache
        .store_verified_pair(
            &new,
            b"new appcast",
            b"new signature",
            DOCUMENT_CAP,
            SIGNATURE_CAP,
        )
        .unwrap();
    assert_eq!(
        cache.load_pair(&old, DOCUMENT_CAP, SIGNATURE_CAP),
        Some((b"old appcast".to_vec(), b"old signature".to_vec()))
    );
    assert_eq!(
        cache.load_pair(&new, DOCUMENT_CAP, SIGNATURE_CAP),
        Some((b"new appcast".to_vec(), b"new signature".to_vec()))
    );
    assert_ne!(
        old.digest(),
        new.digest(),
        "the immutable tag is in the ref key"
    );
}

#[test]
fn exact_index_refs_coexist_and_every_component_is_bounded() {
    let (_dir, layout) = fixture("exact-indexes");
    let cache = SignedArtifactCache::for_layout(&layout);
    let old_key = SignedIndexKey::release("github:owner/index", "atpkg-index-3").unwrap();
    let new_key = SignedIndexKey::release("github:owner/index", "atpkg-index-4").unwrap();
    let old = index_candidate("atpkg-index-3", 3);
    let new = index_candidate("atpkg-index-4", 4);
    for (key, candidate) in [(&old_key, &old), (&new_key, &new)] {
        cache
            .store_verified_index(
                key,
                candidate,
                DOCUMENT_CAP,
                SIGNATURE_CAP,
                ROSTER_CAP,
                SIGNATURE_CAP,
            )
            .unwrap();
    }
    let loaded_old = cache
        .load_index(
            &old_key,
            DOCUMENT_CAP,
            SIGNATURE_CAP,
            ROSTER_CAP,
            SIGNATURE_CAP,
        )
        .unwrap();
    let loaded_new = cache
        .load_index(
            &new_key,
            DOCUMENT_CAP,
            SIGNATURE_CAP,
            ROSTER_CAP,
            SIGNATURE_CAP,
        )
        .unwrap();
    assert_eq!(candidate_parts(&loaded_old), candidate_parts(&old));
    assert_eq!(candidate_parts(&loaded_new), candidate_parts(&new));
    assert_ne!(
        cache.ref_path(&old_key.digest()),
        cache.ref_path(&new_key.digest()),
        "each exact index release retains its own ref"
    );
    for slot in 0..4 {
        let mut caps = [DOCUMENT_CAP, SIGNATURE_CAP, ROSTER_CAP, SIGNATURE_CAP];
        caps[slot] = 1;
        assert!(
            cache
                .load_index(&old_key, caps[0], caps[1], caps[2], caps[3])
                .is_none(),
            "component {slot} remains subject to the caller's current cap"
        );
    }
    assert_eq!(
        cache
            .store_verified_index(
                &old_key,
                &new,
                DOCUMENT_CAP,
                SIGNATURE_CAP,
                ROSTER_CAP,
                SIGNATURE_CAP,
            )
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::InvalidInput,
        "an exact ref cannot be populated by another release label"
    );
}

#[test]
fn an_intact_index_object_cannot_be_relinked_under_another_release() {
    let (_dir, layout) = fixture("index-key-binding");
    let cache = SignedArtifactCache::for_layout(&layout);
    let old_key = SignedIndexKey::release("github:owner/index", "atpkg-index-3").unwrap();
    let new_key = SignedIndexKey::release("github:owner/index", "atpkg-index-4").unwrap();
    for (key, candidate) in [
        (&old_key, index_candidate("atpkg-index-3", 3)),
        (&new_key, index_candidate("atpkg-index-4", 4)),
    ] {
        cache
            .store_verified_index(
                key,
                &candidate,
                DOCUMENT_CAP,
                SIGNATURE_CAP,
                ROSTER_CAP,
                SIGNATURE_CAP,
            )
            .unwrap();
    }
    let new_ref = cache.ref_path(&new_key.digest()).unwrap();
    let new_object = object_digest_from_ref(&new_ref);
    let old_ref = cache.ref_path(&old_key.digest()).unwrap();
    std::fs::write(
        old_ref,
        format!("{REF_HEADER}\n{}\n{new_object}\n", old_key.digest()),
    )
    .unwrap();
    assert!(
        cache
            .load_index(
                &old_key,
                DOCUMENT_CAP,
                SIGNATURE_CAP,
                ROSTER_CAP,
                SIGNATURE_CAP,
            )
            .is_none(),
        "the index envelope is bound to the release key whose label is reconstructed"
    );
}

#[cfg(unix)]
#[test]
fn storing_identical_verified_bytes_keeps_object_and_ref_inodes() {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    let (_dir, layout) = fixture("idempotent");
    let cache = SignedArtifactCache::for_layout(&layout);
    let store = || {
        cache
            .store_verified(
                &key(),
                b"signed manifest",
                b"detached signature",
                DOCUMENT_CAP,
                SIGNATURE_CAP,
            )
            .unwrap();
    };
    store();
    let (reference, object) = paths(&cache);
    let identities = || {
        let reference = std::fs::metadata(&reference).unwrap();
        let object = std::fs::metadata(&object).unwrap();
        (
            (reference.dev(), reference.ino()),
            (object.dev(), object.ino()),
        )
    };
    let before = identities();
    assert!(!layout.is_system_prefix());
    let mode = std::fs::metadata(cache.root())
        .unwrap()
        .permissions()
        .mode();
    let flushes = aterm_durable::seam::Recording::start();
    store();
    assert!(flushes.take().is_empty());
    assert_eq!(flushes.take_plain(), 0);
    assert_eq!(
        std::fs::metadata(cache.root())
            .unwrap()
            .permissions()
            .mode(),
        mode
    );
    assert_eq!(
        identities(),
        before,
        "a warm hit performs no durable rewrite"
    );
}

#[cfg(unix)]
#[test]
fn a_system_prefix_keeps_every_cache_component_readable() {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    let (_dir, layout) = fixture("system-prefix");
    crate::store::system_shape::force(&layout.prefix);
    let cache = SignedArtifactCache::for_layout(&layout);
    let index_key = SignedIndexKey::release("github:owner/index", "atpkg-index-3").unwrap();
    let candidate = index_candidate("atpkg-index-3", 3);
    for index in [false, true] {
        let store = || {
            if index {
                cache.store_verified_index(
                    &index_key,
                    &candidate,
                    DOCUMENT_CAP,
                    SIGNATURE_CAP,
                    ROSTER_CAP,
                    SIGNATURE_CAP,
                )
            } else {
                cache.store_verified(
                    &key(),
                    b"signed manifest",
                    b"detached signature",
                    DOCUMENT_CAP,
                    SIGNATURE_CAP,
                )
            }
            .unwrap();
        };
        store();
        let (reference, object) = if index {
            let reference = cache.ref_path(&index_key.digest()).unwrap();
            let object = cache.object_path_for_index(&index_key).unwrap();
            (reference, object)
        } else {
            paths(&cache)
        };
        let directories = [
            layout.prefix.clone(),
            layout.prefix.join(ROOT),
            cache.root(),
            cache.root().join("objects"),
            cache.root().join("objects/sha256"),
            object.parent().unwrap().to_path_buf(),
            cache.root().join("refs"),
            reference.parent().unwrap().to_path_buf(),
        ];
        let files = [&reference, &object];
        let _held = files.map(|path| std::fs::File::open(path).unwrap());
        let before_bytes = files.map(|path| std::fs::read(path).unwrap());
        let file_identity = || {
            files.map(|path| {
                let metadata = std::fs::metadata(path).unwrap();
                (metadata.dev(), metadata.ino())
            })
        };
        let before_identity = file_identity();
        for directory in &directories {
            assert_eq!(
                std::fs::metadata(directory).unwrap().permissions().mode() & 0o7777,
                0o755,
                "{} remains traversable by the system store's readers",
                directory.display()
            );
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        if index {
            let loaded = cache
                .load_index(
                    &index_key,
                    DOCUMENT_CAP,
                    SIGNATURE_CAP,
                    ROSTER_CAP,
                    SIGNATURE_CAP,
                )
                .unwrap();
            assert_eq!(candidate_parts(&loaded), candidate_parts(&candidate));
        } else {
            assert_eq!(
                cache.load(&key(), DOCUMENT_CAP, SIGNATURE_CAP),
                Some((b"signed manifest".to_vec(), b"detached signature".to_vec()))
            );
        }
        for directory in &directories {
            assert_eq!(
                std::fs::metadata(directory).unwrap().permissions().mode() & 0o7777,
                0o700,
                "a read-only load does not repair permissions"
            );
        }
        store();
        assert_eq!(
            file_identity(),
            before_identity,
            "warm repair retains both files"
        );
        assert_eq!(files.map(|path| std::fs::read(path).unwrap()), before_bytes);
        for directory in &directories {
            assert_eq!(
                std::fs::metadata(directory).unwrap().permissions().mode() & 0o7777,
                0o755,
                "a future store repairs its used shared directories"
            );
        }
        for file in files {
            assert_eq!(
                std::fs::metadata(file).unwrap().permissions().mode() & 0o777,
                0o644,
                "{} remains readable by the system store's readers",
                file.display()
            );
        }
        let directory_versions = || {
            directories.each_ref().map(|path| {
                let metadata = std::fs::metadata(path).unwrap();
                (
                    metadata.dev(),
                    metadata.ino(),
                    metadata.ctime(),
                    metadata.ctime_nsec(),
                )
            })
        };
        let before = directory_versions();
        let flushes = aterm_durable::seam::Recording::start();
        store();
        assert!(
            flushes.take().is_empty(),
            "a healthy warm store needs no flush"
        );
        assert_eq!(flushes.take_plain(), 0);
        assert_eq!(directory_versions(), before, "healthy modes need no chmod");
        assert_eq!(file_identity(), before_identity);
    }
}

#[test]
fn a_changed_exact_index_object_is_a_cache_miss() {
    let (_dir, layout) = fixture("corrupt-index");
    let cache = SignedArtifactCache::for_layout(&layout);
    let key = SignedIndexKey::release("github:owner/index", "atpkg-index-3").unwrap();
    let candidate = index_candidate("atpkg-index-3", 3);
    cache
        .store_verified_index(
            &key,
            &candidate,
            DOCUMENT_CAP,
            SIGNATURE_CAP,
            ROSTER_CAP,
            SIGNATURE_CAP,
        )
        .unwrap();
    let object = cache.object_path_for_index(&key).unwrap();
    let mut changed = std::fs::read(&object).unwrap();
    *changed.last_mut().unwrap() ^= 1;
    std::fs::write(object, changed).unwrap();
    assert!(
        cache
            .load_index(&key, DOCUMENT_CAP, SIGNATURE_CAP, ROSTER_CAP, SIGNATURE_CAP,)
            .is_none(),
        "a changed quad must fall through to its live transport"
    );
}

#[test]
fn a_verified_pair_round_trips_by_logical_ref_and_content_address() {
    let (_dir, layout) = fixture("round-trip");
    let cache = SignedArtifactCache::for_layout(&layout);
    cache
        .store_verified(
            &key(),
            b"signed manifest",
            b"detached signature",
            DOCUMENT_CAP,
            SIGNATURE_CAP,
        )
        .unwrap();
    assert_eq!(
        cache.load(&key(), DOCUMENT_CAP, SIGNATURE_CAP),
        Some((b"signed manifest".to_vec(), b"detached signature".to_vec()))
    );
    let (reference, object) = paths(&cache);
    assert_eq!(
        object.file_name().unwrap().to_str().unwrap(),
        crate::tree::sha256_bytes(&std::fs::read(&object).unwrap())
    );
    assert!(reference.is_file() && object.is_file());
    assert!(
        cache
            .load(
                &SignedArtifactKey::package("github:owner/index", "owner/repo", "gitleaks", 4,),
                DOCUMENT_CAP,
                SIGNATURE_CAP,
            )
            .is_none(),
        "another signed build has another logical ref"
    );
    assert!(
        cache
            .load(&key(), b"signed manifest".len() - 1, SIGNATURE_CAP)
            .is_none(),
        "a cached document is still subject to the caller's bound"
    );
    assert!(
        cache
            .load(&key(), DOCUMENT_CAP, b"detached signature".len() - 1)
            .is_none(),
        "a cached signature is still subject to the caller's bound"
    );
    assert_eq!(
        cache
            .store_verified(
                &key(),
                b"oversized",
                b"signature",
                b"oversized".len() - 1,
                SIGNATURE_CAP,
            )
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::InvalidInput
    );
}

#[test]
fn changed_partial_and_misdirected_cache_files_are_misses() {
    let (_dir, layout) = fixture("corrupt");
    let cache = SignedArtifactCache::for_layout(&layout);
    let store = || {
        cache
            .store_verified(
                &key(),
                b"signed manifest",
                b"detached signature",
                DOCUMENT_CAP,
                SIGNATURE_CAP,
            )
            .unwrap();
    };
    store();
    let (_reference, object) = paths(&cache);
    let mut changed = std::fs::read(&object).unwrap();
    *changed.last_mut().unwrap() ^= 1;
    std::fs::write(&object, changed).unwrap();
    assert!(cache.load(&key(), DOCUMENT_CAP, SIGNATURE_CAP).is_none());

    store();
    let (_, object) = paths(&cache);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&object)
        .unwrap()
        .set_len(OBJECT_MAGIC.len() as u64)
        .unwrap();
    assert!(cache.load(&key(), DOCUMENT_CAP, SIGNATURE_CAP).is_none());

    store();
    let (reference, _) = paths(&cache);
    let mut lines: Vec<String> = std::fs::read_to_string(&reference)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    lines[1] = "0".repeat(64);
    std::fs::write(&reference, format!("{}\n", lines.join("\n"))).unwrap();
    assert!(cache.load(&key(), DOCUMENT_CAP, SIGNATURE_CAP).is_none());

    store();
    let (reference, _) = paths(&cache);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&reference)
        .unwrap()
        .set_len(REF_HEADER.len() as u64)
        .unwrap();
    assert!(cache.load(&key(), DOCUMENT_CAP, SIGNATURE_CAP).is_none());
}

#[cfg(unix)]
#[test]
fn a_final_component_symlink_is_never_a_cache_hit() {
    let (_dir, layout) = fixture("symlink");
    let cache = SignedArtifactCache::for_layout(&layout);
    cache
        .store_verified(
            &key(),
            b"signed manifest",
            b"detached signature",
            DOCUMENT_CAP,
            SIGNATURE_CAP,
        )
        .unwrap();
    let (reference, object) = paths(&cache);
    let held_ref = reference.with_file_name("held-ref");
    std::fs::rename(&reference, &held_ref).unwrap();
    std::os::unix::fs::symlink(&held_ref, &reference).unwrap();
    assert!(cache.load(&key(), DOCUMENT_CAP, SIGNATURE_CAP).is_none());
    std::fs::remove_file(&reference).unwrap();
    std::fs::rename(&held_ref, &reference).unwrap();

    let held_object = object.with_file_name("held-object");
    std::fs::rename(&object, &held_object).unwrap();
    std::os::unix::fs::symlink(&held_object, &object).unwrap();
    assert!(cache.load(&key(), DOCUMENT_CAP, SIGNATURE_CAP).is_none());
}

#[cfg(unix)]
#[test]
fn an_intermediate_shard_symlink_is_neither_read_nor_written_through() {
    let (_dir, layout) = fixture("intermediate-symlink");
    let cache = SignedArtifactCache::for_layout(&layout);
    cache
        .store_verified(
            &key(),
            b"signed manifest",
            b"detached signature",
            DOCUMENT_CAP,
            SIGNATURE_CAP,
        )
        .unwrap();
    let reference = cache.ref_path(&key().digest()).unwrap();
    let shard = reference.parent().unwrap();
    let held = shard.with_file_name("held-shard");
    std::fs::rename(shard, &held).unwrap();
    std::os::unix::fs::symlink(&held, shard).unwrap();

    assert!(
        cache.load(&key(), DOCUMENT_CAP, SIGNATURE_CAP).is_none(),
        "descriptor traversal refuses the linked intermediate directory"
    );
    assert!(
        cache
            .store_verified(
                &key(),
                b"signed manifest",
                b"detached signature",
                DOCUMENT_CAP,
                SIGNATURE_CAP,
            )
            .is_err(),
        "publication cannot be redirected through that directory either"
    );
    assert!(held.join(key().digest()).is_file());
}
