// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::build_cache::{self, fence::Fence, registered};
use std::os::unix::fs::DirBuilderExt as _;
use std::time::{Duration, SystemTime};

struct World {
    _directory: aterm_tempfile::TempDir,
    home: PathBuf,
    owner: PathBuf,
    target: PathBuf,
}

impl World {
    fn new() -> Self {
        let directory = aterm_tempfile::tempdir().unwrap();
        let home = directory.path().canonicalize().unwrap().join("home");
        std::fs::create_dir(&home).unwrap();
        let owner = home.join("source");
        std::fs::create_dir(&owner).unwrap();
        std::fs::write(owner.join("Cargo.toml"), "[workspace]\nmembers=[]\n").unwrap();
        let target = home.join("target.noindex");
        assert!(registered::create_checked(&home, &target, &owner).unwrap());
        Self {
            _directory: directory,
            home,
            owner,
            target,
        }
    }

    fn output(&self, target: &Path) {
        std::fs::create_dir_all(target.join("debug/deps")).unwrap();
        std::fs::create_dir_all(target.join("debug/incremental/unit")).unwrap();
        std::fs::write(target.join("debug/deps/precious"), b"retained output").unwrap();
        std::fs::write(target.join("debug/incremental/unit/state"), b"incremental").unwrap();
        std::fs::write(target.join("debug/.cargo-lock"), b"").unwrap();
    }

    fn path(&self) -> PathBuf {
        self.home
            .join(registered::INDEX)
            .join(format!("{}.json", registered::key(&self.target).unwrap()))
    }

    fn reclaim(&self) -> Vec<(PathBuf, build_cache::Unused)> {
        let clear = |_: &[PathBuf], _: &[u32]| Ok(aterm_uds::holders::PathScan::default());
        registered::reclaim_on(
            &self.home,
            SystemTime::now() + Duration::from_secs(4 * 24 * 60 * 60),
            build_cache::ReclaimAfter::CALM,
            None,
            &mut |_, _| true,
            &Fence::new(&clear),
        )
    }
}

fn pinned(root: &Path) -> aterm_dirfd::OwnedFd {
    aterm_dirfd::openat(
        aterm_dirfd::CWD,
        root,
        aterm_dirfd::OFlags::RDONLY
            | aterm_dirfd::OFlags::DIRECTORY
            | aterm_dirfd::OFlags::NOFOLLOW
            | aterm_dirfd::OFlags::CLOEXEC,
        aterm_dirfd::Mode::empty(),
    )
    .unwrap()
}

#[test]
fn an_abandoned_walk_fence_preserves_ordinary_custody_and_stops_the_next_pass() {
    let world = World::new();
    world.output(&world.target);
    let before = registered::read(&world.path()).unwrap();
    let previous = registered::read(&world.target.join(MARKER)).unwrap();
    let root = pinned(&world.target);
    let fence = WalkFence::arm(&root, &world.target, || false).unwrap();
    let marker = registered::read(&world.target.join(MARKER)).unwrap();
    assert_eq!(marker.schema, 4);
    assert!(marker.retained);
    let mut original = marker.clone();
    original.schema = previous.schema;
    original.retained = previous.retained;
    original.removal_fence = None;
    assert_eq!(original, previous);
    assert_eq!(registered::read(&world.path()).unwrap(), before);
    assert!(!allows_at(&root));
    assert!(fence.owns(&root));
    drop(fence); // Simulate loss of the only pass-local permission, without a cleanup drop.
    assert!(crate::build_cache::remove::tree_while(&world.target, &|| true).is_err());
    assert_eq!(
        registered::read(&world.target.join(MARKER)).unwrap(),
        marker
    );
    assert_eq!(
        std::fs::read(world.target.join("debug/deps/precious")).unwrap(),
        b"retained output"
    );
}

#[test]
fn an_explicit_keep_revokes_a_live_walk_fence_before_acknowledgement() {
    let model = aterm_models_atpkg::atpkg_deep_removal_fence_model();
    assert!(aterm_spec::interp::violated_invariants(&model).is_empty());
    for (mutant, invariant) in [
        (1, "EveryEscapeAlreadyFenced"),
        (2, "ExplicitKeepRevokesExemption"),
        (3, "EveryEscapeAlreadyFenced"),
    ] {
        assert!(
            aterm_spec::interp::violated_invariants(&aterm_spec::interp::with_buggy(
                &model, mutant
            ))
            .contains(invariant)
        );
    }
    for registered in [false, true] {
        let mut state = model.init_state();
        let world = World::new();
        let target = if registered {
            world.target.clone()
        } else {
            let root = world.home.join("ordinary.noindex");
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&root)
                .unwrap();
            root
        };
        world.output(&target);
        let root = pinned(&target);
        let fence = WalkFence::arm(&root, &target, || false).unwrap();
        assert!(model.fire("Arm", &mut state));
        assert_eq!(i64::from(!allows_at(&root)), state["fence"]);
        assert_eq!(i64::from(fence.owns(&root)), state["owned"]);
        assert!(fence.owns(&root));
        retain(&world.home, &target).unwrap();
        assert!(model.fire("ExplicitKeep", &mut state));
        assert_eq!(i64::from(!allows_at(&root)), state["fence"]);
        assert_eq!(i64::from(fence.owns(&root)), state["owned"]);
        let kept = registered::read(&target.join(MARKER)).unwrap();
        assert!(kept.retained && kept.removal_fence.is_none());
        assert!(!fence.owns(&root));
        assert!(
            !fence.disarm(&root),
            "must not restore over the acknowledged keep"
        );
        assert_eq!(registered::read(&target.join(MARKER)).unwrap(), kept);
        assert!(crate::build_cache::remove::tree_while(&target, &|| true).is_err());
        assert_eq!(
            std::fs::read(target.join("debug/deps/precious")).unwrap(),
            b"retained output"
        );
    }
}

#[test]
fn a_walk_fence_restores_only_its_exact_marker_inode_and_bytes() {
    let world = World::new();
    let root = pinned(&world.target);
    let previous = std::fs::read(world.target.join(MARKER)).unwrap();
    let fence = WalkFence::arm(&root, &world.target, || false).unwrap();
    assert!(fence.disarm(&root));
    assert_eq!(std::fs::read(world.target.join(MARKER)).unwrap(), previous);
    assert!(
        allows_at(&root),
        "ordinary custody keeps its original semantics"
    );

    let fence = WalkFence::arm(&root, &world.target, || false).unwrap();
    let bytes = std::fs::read(world.target.join(MARKER)).unwrap();
    let replacement = world.target.join("replacement");
    std::fs::write(&replacement, &bytes).unwrap();
    std::fs::rename(&replacement, world.target.join(MARKER)).unwrap();
    assert!(
        !fence.owns(&root),
        "identical bytes at a different inode are not ours"
    );
    assert!(!fence.disarm(&root));
    assert_eq!(std::fs::read(world.target.join(MARKER)).unwrap(), bytes);
    assert!(!allows_at(&root));
}

/// A staged payload still carries its pre-staging marker: Pending belongs to the index.
/// Retention must revoke a provisional pass without losing that Pending authority, and
/// without accepting a different original declaration binding.
#[test]
fn retaining_a_fenced_pending_payload_preserves_the_index_only_pending_fields() {
    for authorized in [false, true] {
        let world = World::new();
        world.output(&world.target);
        let stage = world.home.join("retiring.noindex");
        registered::stage_for_process_test(&world.home, &world.target, &stage);
        let mut before = registered::read(&world.path()).unwrap();
        before.pending.as_mut().unwrap().authorized = authorized;
        before.owners_unknown = true;
        registered::write(&world.path(), &before).unwrap();
        let payload = stage.join(registered::PAYLOAD);
        assert!(
            registered::read(&payload.join(MARKER))
                .unwrap()
                .pending
                .is_none()
        );
        let root = pinned(&payload);
        let fence = WalkFence::arm(&root, &payload, || false).unwrap();
        retain(&world.home, &payload).unwrap();
        let kept = registered::read(&world.path()).unwrap();
        let mut expected = before;
        expected.schema = 4;
        expected.retained = true;
        expected.removal_fence = None;
        assert_eq!(kept, expected);
        assert!(!fence.owns(&root));
        assert!(!fence.disarm(&root));
        assert!(world.reclaim().is_empty());
        assert_eq!(
            std::fs::read(payload.join("debug/deps/precious")).unwrap(),
            b"retained output"
        );
    }

    let world = World::new();
    world.output(&world.target);
    let stage = world.home.join("different.noindex");
    registered::stage_for_process_test(&world.home, &world.target, &stage);
    let payload = stage.join(registered::PAYLOAD);
    let root = pinned(&payload);
    let _fence = WalkFence::arm(&root, &payload, || false).unwrap();
    let mut changed = registered::read(&payload.join(MARKER)).unwrap();
    changed.nonce = "00000000000000000000000000000000".into();
    registered::write(&payload.join(MARKER), &changed).unwrap();
    let index_before = std::fs::read(world.path()).unwrap();
    let marker_before = std::fs::read(payload.join(MARKER)).unwrap();
    assert!(retain(&world.home, &payload).is_err());
    assert_eq!(std::fs::read(world.path()).unwrap(), index_before);
    assert_eq!(std::fs::read(payload.join(MARKER)).unwrap(), marker_before);
}

/// A real registered retirement predicate reads its unchanged index and the payload's
/// ancestors. Its own top fence must not revoke that ordinary, normally eligible deep walk.
#[test]
fn an_ordinary_pending_deep_retirement_does_not_revoke_its_own_fence() {
    let world = World::new();
    world.output(&world.target);
    let mut deep = world.target.clone();
    for _ in 0..140 {
        deep.push("d");
        std::fs::create_dir(&deep).unwrap();
    }
    std::fs::write(deep.join("unit"), b"eligible registered deep output").unwrap();
    let stage = world.home.join("deep-retiring.noindex");
    registered::stage_for_process_test(&world.home, &world.target, &stage);
    let receipt = registered::read(&world.path()).unwrap();
    let pending = receipt.pending.as_ref().unwrap();
    let result = registered::remove_pending(&world.path(), &receipt, pending, None).unwrap();
    assert!(result.whole());
    assert!(!stage.exists());
    assert_eq!(registered::read(&world.path()).unwrap(), receipt);
}

/// Mode bits cannot conceal a macOS write grant. Neither a writable directory's marker
/// nor a marker with a newly granted writer may authorize the pass-local exemption.
#[cfg(target_os = "macos")]
#[test]
fn a_walk_fence_refuses_directory_and_marker_acl_grants_through_the_held_files() {
    use crate::build_cache::tests::{acl, holds_ace};

    let world = World::new();
    let root = pinned(&world.target);
    acl(&world.target, &["group:everyone allow add_file"]);
    let before = std::fs::read(world.target.join(MARKER)).unwrap();
    assert!(holds_ace(&world.target, "group:everyone allow add_file"));
    assert!(WalkFence::arm(&root, &world.target, || false).is_none());
    assert!(retain(&world.home, &world.target).is_err());
    assert!(holds_ace(&world.target, "group:everyone allow add_file"));
    assert_eq!(std::fs::read(world.target.join(MARKER)).unwrap(), before);

    let other = World::new();
    let root = pinned(&other.target);
    let fence = WalkFence::arm(&root, &other.target, || false).unwrap();
    let marker = other.target.join(MARKER);
    acl(&marker, &["group:everyone allow write"]);
    let before = std::fs::read(&marker).unwrap();
    assert!(!fence.owns(&root));
    assert!(!fence.disarm(&root));
    assert!(holds_ace(&marker, "group:everyone allow write"));
    assert_eq!(std::fs::read(marker).unwrap(), before);
}

#[test]
fn retained_registered_root_keeps_whole_and_partial_bytes_while_siblings_reclaim() {
    for owner_gone in [false, true] {
        let world = World::new();
        world.output(&world.target);
        let sibling = world.home.join("sibling.noindex");
        assert!(registered::create_checked(&world.home, &sibling, &world.owner).unwrap());
        world.output(&sibling);
        let before = registered::read(&world.path()).unwrap();
        retain(&world.home, &world.target).unwrap();
        let kept = registered::read(&world.path()).unwrap();
        assert_eq!(kept.schema, 4);
        assert!(kept.retained);
        assert_eq!(kept.identity, before.identity);
        assert_eq!(kept.owners, before.owners);
        assert_eq!(kept.owners_unknown, before.owners_unknown);
        assert_eq!(kept.pending, before.pending);
        assert_eq!(kept.package, before.package);
        if owner_gone {
            std::fs::remove_dir_all(&world.owner).unwrap();
        }
        let removed = world.reclaim();
        assert!(
            !removed.is_empty(),
            "the non-retained sibling still reclaims"
        );
        assert_eq!(
            std::fs::read(world.target.join("debug/deps/precious")).unwrap(),
            b"retained output"
        );
        assert!(world.target.join("debug/incremental/unit/state").is_file());
        assert!(!sibling.join("debug/incremental/unit/state").exists());
        assert!(
            registered::plan(
                &world.home,
                SystemTime::now() + Duration::from_secs(40 * 24 * 60 * 60)
            )
            .iter()
            .all(|candidate| candidate.path != world.target)
        );
    }
}

#[test]
fn existing_unregistered_retention_creates_only_keep_authority_and_preserves_siblings() {
    let world = World::new();
    let root = world.home.join("unregistered.noindex");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&root)
        .unwrap();
    world.output(&root);
    retain(&world.home, &root).unwrap();
    let receipt = registered::read(&root.join(MARKER)).unwrap();
    assert!(receipt.retained && receipt.owners_unknown);
    assert!(receipt.owners.is_empty() && receipt.package.is_none());
    assert!(registered::ordinary_clearance(&world.home, &root).is_err());
    assert!(registered::ordinary_clearance(&world.home, &root.join("debug/deps")).is_err());
    assert!(registered::ordinary_clearance(&world.home, &world.home.join("unrelated")).is_ok());
    assert!(!allows_tree(&root.join("debug/deps")));
    assert!(allows_tree(&world.home.join("unrelated")));
    world.reclaim();
    assert!(root.join("debug/deps/precious").is_file());
}

#[test]
fn retaining_a_pending_payload_keeps_its_original_receipt_and_pending_authority() {
    let world = World::new();
    world.output(&world.target);
    let stage = world.home.join("retiring.noindex");
    registered::stage_for_process_test(&world.home, &world.target, &stage);
    let before = registered::read(&world.path()).unwrap();
    let payload = stage.join("cache.noindex");
    retain(&world.home, &payload).unwrap();
    let kept = registered::read(&world.path()).unwrap();
    assert!(kept.retained);
    assert_eq!(kept.pending, before.pending);
    assert_eq!(kept.root, before.root);
    assert_eq!(kept.identity, before.identity);
    assert!(world.reclaim().is_empty());
    assert!(payload.join("debug/deps/precious").is_file());
}

/// A pending payload hiding its marker keeps its permissions and bytes. Without search,
/// neither the marker nor its complete Cargo-lock inventory can be established, so no
/// removal intent starts. Restoring search allows the same authorized payload to retire.
#[test]
fn a_pending_payload_hiding_custody_stays_unchanged_until_search_is_available() {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let world = World::new();
    world.output(&world.target);
    let stage = world.home.join("retiring.noindex");
    registered::stage_for_process_test(&world.home, &world.target, &stage);
    let mut receipt = registered::read(&world.path()).unwrap();
    receipt.pending.as_mut().unwrap().authorized = true;
    registered::write(&world.path(), &receipt).unwrap();
    let recorded = std::fs::read(world.path()).unwrap();
    let payload = stage.join("cache.noindex");
    let marker = std::fs::read(payload.join(MARKER)).unwrap();
    std::fs::set_permissions(&payload, std::fs::Permissions::from_mode(0o600)).unwrap();
    let before = std::fs::symlink_metadata(&payload).unwrap();
    assert!(
        std::fs::symlink_metadata(payload.join(MARKER))
            .is_err_and(|e| e.kind() != std::io::ErrorKind::NotFound)
    );

    let mut asked = Vec::new();
    let clear = |_: &[PathBuf], _: &[u32]| Ok(aterm_uds::holders::PathScan::default());
    let removed = registered::reclaim_on(
        &world.home,
        SystemTime::now() + Duration::from_secs(4 * 24 * 60 * 60),
        build_cache::ReclaimAfter::CALM,
        None,
        &mut |at, why| {
            asked.push((at.to_path_buf(), why));
            true
        },
        &Fence::new(&clear),
    );
    assert!(removed.is_empty());
    assert!(
        asked.is_empty(),
        "unknown Cargo locks admit no removal intent"
    );
    assert_eq!(std::fs::read(world.path()).unwrap(), recorded);
    let after = std::fs::symlink_metadata(&payload).unwrap();
    assert_eq!(
        (after.ino(), after.mode(), after.ctime(), after.ctime_nsec()),
        (
            before.ino(),
            before.mode(),
            before.ctime(),
            before.ctime_nsec()
        )
    );
    assert!(stage.exists());
    std::fs::set_permissions(&payload, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(std::fs::read(payload.join(MARKER)).unwrap(), marker);
    assert_eq!(
        std::fs::read(payload.join("debug/deps/precious")).unwrap(),
        b"retained output"
    );
    assert_eq!(
        world.reclaim(),
        [(stage.clone(), build_cache::Unused::Unfinished)]
    );
    assert!(!payload.exists() && !stage.exists());
}

/// AN ORDINARY MARKER BINDS ONLY A PRIVATE FOLDER, THROUGH ITS DESCRIPTOR AS BY ITS PATH: a
/// registered root's own marker (schema 1, never retained), moved with its root to a stage,
/// allows the stage's removal while the stage is private, read through the folder held
/// ([`allows_at`]) as by its path ([`allows_tree`]). Once the stage is writable by its group, or
/// by others, both read the marker as binding no folder, and a removal of the stage gated as a
/// sweep gates one ([`allows_walk`]) is revoked with what it holds intact. NEGATIVE CONTROL: the
/// private stage goes whole.
#[test]
fn an_ordinary_marker_binds_only_a_private_folder_through_its_descriptor_too() {
    use crate::build_cache::remove::tree_while;
    use std::os::unix::fs::PermissionsExt as _;
    let world = World::new();
    world.output(&world.target);
    let stage = world.home.join("stage");
    std::fs::rename(&world.target, &stage).unwrap();
    let held = std::fs::File::open(&stage).unwrap();
    assert!(allows_tree(&stage) && allows_at(&held));
    for open in [0o770, 0o702] {
        std::fs::set_permissions(&stage, std::fs::Permissions::from_mode(open)).unwrap();
        assert!(!allows_tree(&stage), "{open:o}");
        assert!(!allows_at(&held), "{open:o}");
        let walked = tree_while(&stage, &|| allows_walk(&stage));
        assert!(walked.is_err(), "{open:o}: {walked:?}");
        assert_eq!(
            std::fs::read(stage.join("debug/deps/precious")).unwrap(),
            b"retained output",
            "{open:o}"
        );
    }
    std::fs::set_permissions(&stage, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(allows_at(&held));
    drop(held);
    assert!(tree_while(&stage, &|| allows_walk(&stage)).unwrap().whole());
    assert!(!stage.exists());
}

#[test]
fn unreadable_custody_inside_an_open_pending_tree_revokes_descriptor_mutations() {
    use crate::build_cache::remove::seams;
    use std::cell::Cell;
    use std::rc::Rc;
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            seams::clear();
        }
    }
    let world = World::new();
    world.output(&world.target);
    std::fs::create_dir_all(world.target.join("remaining/deep")).unwrap();
    std::fs::write(
        world.target.join("remaining/deep/precious"),
        b"keep remainder",
    )
    .unwrap();
    let stage = world.home.join("retiring.noindex");
    registered::stage_for_process_test(&world.home, &world.target, &stage);
    let mut receipt = registered::read(&world.path()).unwrap();
    receipt.pending.as_mut().unwrap().authorized = true;
    registered::write(&world.path(), &receipt).unwrap();
    let payload = stage.join("cache.noindex");
    let unknown_remaining = payload.join("remaining");
    let observed = Rc::new(Cell::new(false));
    let seen = Rc::clone(&observed);
    let _clear = Clear;
    seams::on(seams::At::Opening, move |name| {
        if name == "deep" && !seen.replace(true) {
            // This models newly unreadable custody, not a supported nested admission.
            // The API refuses overlapping unregistered keeps before publication.
            std::fs::write(unknown_remaining.join(MARKER), b"unreadable custody").unwrap();
        }
    });
    let removed = world.reclaim();
    assert!(observed.get());
    assert!(
        removed
            .iter()
            .any(|(_, why)| matches!(why, build_cache::Unused::Revoked { .. }))
    );
    assert_eq!(
        std::fs::read(payload.join("remaining/deep/precious")).unwrap(),
        b"keep remainder"
    );
    assert!(registered::read(&world.path()).unwrap().pending.is_some());
}

#[test]
fn unknown_or_replaced_retention_never_releases_custody() {
    let world = World::new();
    world.output(&world.target);
    retain(&world.home, &world.target).unwrap();
    let receipt = registered::read(&world.path()).unwrap();
    std::fs::write(world.target.join(MARKER), b"unreadable declaration").unwrap();
    assert!(registered::custody_present(&world.target));
    assert!(!allows_tree(&world.target));
    assert!(world.reclaim().is_empty());
    assert!(retain(&world.home, &world.target).is_err());
    assert_eq!(registered::read(&world.path()).unwrap(), receipt);
    let old = world.home.join("old.noindex");
    std::fs::rename(&world.target, &old).unwrap();
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&world.target)
        .unwrap();
    assert!(retain(&world.home, &world.target).is_err());
    assert!(old.join("debug/deps/precious").is_file());
}

#[test]
fn schema4_is_refused_by_the_installed_0147_registered_reader() {
    let world = World::new();
    // The exact 5bb7817df4f412e95027cf03e8c37541494c8187 reader accepts only
    // schema1/no-package or schema2/valid-package. This is a behavioral omission control,
    // not a claim that its ordinary target cleaner already honored custody markers.
    let old_accepts = |receipt: &Receipt| {
        (receipt.schema == 1 && receipt.package.is_none())
            || (receipt.schema == 2
                && receipt
                    .package
                    .as_ref()
                    .is_some_and(super::super::package::Policy::valid))
    };
    let before = registered::read(&world.path()).unwrap();
    assert!(old_accepts(&before));
    retain(&world.home, &world.target).unwrap();
    let kept = registered::read(&world.path()).unwrap();
    assert!(!old_accepts(&kept));
    let mut ignored_field_mutant = kept.clone();
    ignored_field_mutant.schema = 1;
    assert!(
        old_accepts(&ignored_field_mutant),
        "an additive field without schema bump is unsafe"
    );
}

#[test]
fn retain_refuses_missing_source_legacy_placements_and_busy_registration() {
    let world = World::new();
    world.output(&world.target);
    assert!(retain(&world.home, &world.target.join("debug/incremental/unit")).is_err());
    assert!(
        !world
            .target
            .join("debug/incremental/unit")
            .join(MARKER)
            .exists()
    );
    assert!(retain(&world.home, &world.home.join("missing.noindex")).is_err());
    assert!(retain(&world.home, &world.owner).is_err());
    let legacy = world.home.join("old-verify-cells.noindex");
    std::fs::create_dir(&legacy).unwrap();
    assert!(retain(&world.home, &legacy).is_err());
    let gate = registered::gate_path(&world.path()).unwrap();
    let _held =
        crate::lock::Flock::try_lock(crate::metadata_io::open_regular(&gate).unwrap()).unwrap();
    assert!(retain(&world.home, &world.target).is_err());
    assert!(!registered::read(&world.path()).unwrap().retained);
}
