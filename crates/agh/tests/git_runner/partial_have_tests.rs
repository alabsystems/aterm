// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Agh's own metadata-only snapshots are object data, not complete fetch haves.

use super::*;

struct KeptWorld(Option<World>);

impl std::ops::Deref for KeptWorld {
    type Target = World;
    fn deref(&self) -> &World {
        self.0.as_ref().unwrap()
    }
}

impl Drop for KeptWorld {
    fn drop(&mut self) {
        if std::thread::panicking()
            && let Some(world) = self.0.take()
        {
            eprintln!(
                "partial-have world retained: {}",
                world.root.keep().display()
            );
        }
    }
}

/// The commit reader can retain a real commit without its tree. Neither raw,
/// named nor batch acquisition may equate that object with a complete source.
#[test]
fn commit_only_objects_do_not_shortcut_raw_named_or_batch_source_completion() {
    let world = KeptWorld(Some(World::canonical("git-commit-only-have")));
    let bare = bare_repo(world.path(), "app");
    let work = world.path().join("app-work");
    std::fs::write(
        work.join("payload"),
        b"actual absent checkout tree and body\n",
    )
    .unwrap();
    host_git(&work, &["add", "payload"]);
    host_git(
        &work,
        &["commit", "--quiet", "-m", "physical selected tree"],
    );
    host_git(&work, &["push", "--quiet", bare.to_str().unwrap(), "main"]);
    let oid = host_git(&bare, &["rev-parse", "main"]);
    let tree = host_git(&bare, &["rev-parse", "main^{tree}"]);
    let raw = super::raw_blob_tests::git_input(&bare, &["cat-file", "commit", &oid], None);
    assert!(raw.status.success());
    let gh = world
        .client(git_env())
        .git_remote_for_tests("fixture-dev/app", &bare)
        .build();
    let slug = common::slug("fixture-dev/app");
    for operation in ["raw", "named", "batch", "batch-oid"] {
        let target = world.path().join(format!("{operation}.git"));
        host_git(
            world.path(),
            &["init", "--quiet", "--bare", target.to_str().unwrap()],
        );
        let inserted = super::raw_blob_tests::git_input(
            &target,
            &["hash-object", "-t", "commit", "-w", "--stdin"],
            Some(&raw.stdout),
        );
        assert!(inserted.status.success());
        assert_eq!(String::from_utf8(inserted.stdout).unwrap().trim_end(), oid);
        let missing = super::raw_blob_tests::git_input(&target, &["cat-file", "-t", &tree], None);
        assert!(
            !missing.status.success(),
            "the original root tree must really be absent"
        );
        assert!(host_git(&target, &["for-each-ref"]).is_empty());
        let request = match operation {
            "raw" => GitRun::fetch_ref(
                &slug,
                &target,
                &oid,
                "refs/agh/complete",
                true,
                agh::git::FetchScope::NativeExact,
            ),
            "named" => GitRun::fetch_ref(
                &slug,
                &target,
                "refs/heads/main",
                "refs/agh/complete",
                true,
                agh::git::FetchScope::NativeExact,
            ),
            "batch" => GitRun::fetch_batch(
                &slug,
                &target,
                fetch_rows(&[("refs/heads/main", "refs/agh/complete", &oid)]),
                true,
            ),
            "batch-oid" => GitRun::fetch_batch(
                &slug,
                &target,
                fetch_rows(&[(&oid, "refs/agh/complete", &oid)]),
                true,
            ),
            _ => unreachable!(),
        };
        if let Err(error) = gh.run_git(&request) {
            assert!(
                host_git(&target, &["for-each-ref"]).is_empty(),
                "failure must publish no ref"
            );
            panic!("{operation} commit-only acquisition: {error:?}");
        }
        assert_eq!(host_git(&target, &["rev-parse", "refs/agh/complete"]), oid);
        assert!(
            !host_git(&target, &["rev-list", "--objects", &oid, "--missing=print"])
                .lines()
                .any(|line| line.starts_with('?'))
        );
    }
}

/// A real metadata acquisition can outlive its cache ref. Git's raw-OID
/// optimization must not mistake that surviving commit for its complete graph.
#[test]
fn unreferenced_metadata_commit_does_not_shortcut_exact_source_completion() {
    let world = KeptWorld(Some(World::canonical("git-unreferenced-partial-have")));
    let bare = bare_repo(world.path(), "app");
    let work = world.path().join("app-work");
    std::fs::write(
        work.join("payload"),
        b"required original blob absent from metadata snapshots\n",
    )
    .unwrap();
    host_git(&work, &["add", "payload"]);
    host_git(&work, &["commit", "--quiet", "-m", "original payload"]);
    host_git(&work, &["push", "--quiet", bare.to_str().unwrap(), "main"]);
    host_git(&bare, &["config", "uploadpack.allowFilter", "true"]);
    let oid = host_git(&bare, &["rev-parse", "main"]);
    let blob = host_git(&bare, &["rev-parse", "main:payload"]);
    let slug = common::slug("fixture-dev/app");
    let gh = world
        .client(git_env())
        .git_remote_for_tests("fixture-dev/app", &bare)
        .build();
    for mode in ["cold", "full", "unreferenced-metadata"] {
        let target = world.path().join(format!("{mode}.git"));
        host_git(
            world.path(),
            &["init", "--quiet", "--bare", target.to_str().unwrap()],
        );
        if mode != "cold" {
            let request = GitRun::fetch_main(&slug, &target, "refs/agh/seed");
            gh.run_git(&if mode == "full" {
                request
            } else {
                request.without_blobs()
            })
            .unwrap();
            host_git(&target, &["update-ref", "-d", "refs/agh/seed"]);
            assert!(host_git(&target, &["for-each-ref"]).is_empty());
            assert_eq!(host_git(&target, &["cat-file", "-t", &oid]), "commit");
            if mode == "unreferenced-metadata" {
                assert!(
                    host_git(&target, &["rev-list", "--objects", &oid, "--missing=print"])
                        .contains(&format!("?{blob}"))
                );
            }
        }
        let before = host_git(&target, &["for-each-ref"]);
        if let Err(error) = gh.run_git(&GitRun::fetch_ref(
            &slug,
            &target,
            &oid,
            "refs/agh/complete",
            true,
            agh::git::FetchScope::NativeExact,
        )) {
            assert_eq!(
                host_git(&target, &["for-each-ref"]),
                before,
                "failed exact source cannot publish a ref"
            );
            panic!(
                "{mode} exact source failed: {error:?}; final child: {:?}",
                gh.fx().children_run().last()
            );
        }
        assert_eq!(host_git(&target, &["rev-parse", "refs/agh/complete"]), oid);
        assert_eq!(host_git(&target, &["cat-file", "-t", &blob]), "blob");
        assert!(
            !host_git(&target, &["rev-list", "--objects", &oid, "--missing=print"])
                .lines()
                .any(|line| line.starts_with('?'))
        );
    }
}

/// Produce the partial store through the real public metadata operation, then ask
/// ordinary main/exact/batch fetch to complete the same history. Cold and fully populated
/// stores exercise the identical native request as positive controls.
#[test]
fn metadata_only_haves_do_not_omit_required_historical_blobs() {
    let world = KeptWorld(Some(World::canonical("git-partial-haves")));
    let bare = bare_repo(world.path(), "app");
    let work = world.path().join("app-work");
    let mut bytes = String::new();
    for line in 0..8192 {
        use std::fmt::Write as _;
        writeln!(
            bytes,
            "historical content {line:08} abcdefghijklmnopqrstuvwxyz"
        )
        .unwrap();
    }
    std::fs::write(work.join("payload"), &bytes).unwrap();
    host_git(&work, &["add", "payload"]);
    host_git(&work, &["commit", "--quiet", "-m", "historical payload"]);
    host_git(&work, &["push", "--quiet", &bare.to_string_lossy(), "main"]);
    host_git(&bare, &["config", "uploadpack.allowFilter", "true"]);
    let old = host_git(&bare, &["rev-parse", "main"]);
    let historical = host_git(&bare, &["rev-parse", "main:payload"]);
    let slug = common::slug("fixture-dev/app");
    let gh = world
        .client(git_env())
        .git_remote_for_tests("fixture-dev/app", &bare)
        .build();
    let mut targets = Vec::new();
    for (mode, operation) in ["full", "cold", "metadata"]
        .into_iter()
        .flat_map(|mode| ["main", "exact", "batch"].map(|operation| (mode, operation)))
    {
        let target = world.path().join(format!("{mode}-{operation}.git"));
        host_git(
            world.path(),
            &["init", "--quiet", "--bare", target.to_str().unwrap()],
        );
        if mode == "full" {
            gh.run_git(&GitRun::fetch_main(&slug, &target, "refs/agh/exact"))
                .unwrap();
        } else if mode == "metadata" {
            gh.run_git(&GitRun::fetch_main(&slug, &target, "refs/agh/exact").without_blobs())
                .unwrap();
            assert_eq!(
                host_git(&target, &["rev-parse", "--is-shallow-repository"]),
                "false"
            );
            assert_eq!(host_git(&target, &["rev-parse", "refs/agh/exact"]), old);
            assert!(
                host_git(
                    &target,
                    &["rev-list", "--objects", "--all", "--missing=print"]
                )
                .contains(&format!("?{historical}"))
            );
            assert!(
                !std::fs::read_to_string(target.join("config"))
                    .unwrap()
                    .contains("promisor")
            );
        }
        targets.push((mode, operation, target));
    }
    // The next version is deliberately close to the omitted historical body.
    // Real upload-pack chooses its delta representation; the assertion below
    // checks object completeness rather than a particular pack heuristic.
    bytes.push_str("the upstream edit\n");
    std::fs::write(work.join("payload"), bytes).unwrap();
    host_git(&work, &["commit", "--quiet", "-am", "upstream payload"]);
    host_git(&work, &["push", "--quiet", &bare.to_string_lossy(), "main"]);
    let new = host_git(&bare, &["rev-parse", "main"]);
    for (mode, operation, target) in &targets {
        let before = host_git(target, &["for-each-ref"]);
        let request = match *operation {
            "main" => GitRun::fetch_main(&slug, target, "refs/agh/exact"),
            "batch" => GitRun::fetch_batch(
                &slug,
                target,
                fetch_rows(&[("refs/heads/main", "refs/agh/exact", &new)]),
                true,
            ),
            "exact" => GitRun::fetch_ref(
                &slug,
                target,
                "refs/heads/main",
                "refs/agh/exact",
                true,
                agh::git::FetchScope::NativeExact,
            ),
            _ => unreachable!("test operation"),
        };
        if let Err(error) = gh.run_git(&request) {
            assert_eq!(
                host_git(target, &["for-each-ref"]),
                before,
                "failed fetch moved refs"
            );
            panic!(
                "{mode} operation={operation} fetch: {error:?}; final child: {:?}",
                gh.fx().children_run().last()
            );
        }
        assert_eq!(host_git(target, &["rev-parse", "refs/agh/exact"]), new);
        let expected_reflog = if *mode == "cold" {
            new.clone()
        } else {
            format!("{new}\n{old}")
        };
        assert_eq!(
            host_git(target, &["reflog", "show", "--format=%H", "refs/agh/exact"]),
            expected_reflog
        );
        let objects = host_git(target, &["rev-list", "--objects", &new, "--missing=print"]);
        assert!(
            !objects.lines().any(|line| line.starts_with('?')),
            "{mode}: {objects}"
        );
        assert_eq!(host_git(target, &["cat-file", "-t", &historical]), "blob");
    }
}

/// Checkout bodies are fetched from the exact commit in a separate cold repository.
/// Exercise no cache, a complete cache and a metadata-only cache on one provider.
/// Only a cache miss fetches bodies: a complete cache sends no body fetch, so a
/// presence read that saw nothing (and answered every blob missing) fails here.
#[test]
fn checkout_closure_fetches_real_blob_objects_with_and_without_a_cache() {
    use std::os::unix::fs::PermissionsExt as _;

    let world = KeptWorld(Some(World::canonical("git-checkout-blob-wants")));
    let bare = bare_repo(world.path(), "app");
    let work = world.path().join("app-work");
    std::fs::write(
        work.join("payload"),
        b"historical blob outside the selected tree\n",
    )
    .unwrap();
    host_git(&work, &["add", "payload"]);
    host_git(&work, &["commit", "--quiet", "-m", "historical payload"]);
    let historical_commit = host_git(&work, &["rev-parse", "HEAD"]);
    let historical_blob = host_git(&work, &["rev-parse", "HEAD:payload"]);
    std::fs::write(work.join("payload"), b"the selected blob body\n").unwrap();
    host_git(&work, &["add", "payload"]);
    host_git(&work, &["commit", "--quiet", "-m", "selected payload"]);
    host_git(&work, &["push", "--quiet", &bare.to_string_lossy(), "main"]);
    host_git(&bare, &["config", "uploadpack.allowFilter", "true"]);
    let oid = host_git(&bare, &["rev-parse", "main"]);
    let blob = host_git(&bare, &["rev-parse", "main:payload"]);
    let gh = world
        .client(git_env())
        .git_remote_for_tests("fixture-dev/app", &bare)
        .build();
    let slug = common::slug("fixture-dev/app");
    let partial = world.path().join("partial-cache.git");
    host_git(
        world.path(),
        &["init", "--quiet", "--bare", partial.to_str().unwrap()],
    );
    gh.run_git(&GitRun::fetch_main(&slug, &partial, "refs/agh/metadata").without_blobs())
        .unwrap();
    let body_fetches = || {
        gh.fx()
            .children_run()
            .iter()
            .filter(|child| {
                child.args.iter().any(|arg| arg == "--depth=1")
                    // Native refetch exports checked cache DATA locally; only
                    // the remote body request counts as a provider fetch.
                    && !child.args.iter().any(|arg| arg == "--refetch")
            })
            .count()
    };
    for (label, cache, fetches) in [
        ("cold", None, true),
        ("full", Some(bare.as_path()), false),
        ("partial", Some(partial.as_path()), true),
    ] {
        let fetched_before = body_fetches();
        let target = world.path().join(format!("target-{label}.git"));
        host_git(
            world.path(),
            &["init", "--quiet", "--bare", target.to_str().unwrap()],
        );
        let request = GitRun::fetch_ref(
            &slug,
            &target,
            &oid,
            "refs/agh/selected",
            true,
            agh::git::FetchScope::SelectedCheckout {
                cache: cache.map(|path| path.to_path_buf()),
            },
        );
        gh.run_git(&request).unwrap_or_else(|error| {
            panic!(
                "cache={cache:?}: {error:?}; final child: {:?}",
                gh.fx().children_run().last()
            )
        });
        assert_eq!(host_git(&target, &["cat-file", "-t", &blob]), "blob");
        assert_eq!(host_git(&target, &["rev-parse", "refs/agh/selected"]), oid);
        assert_eq!(
            body_fetches() - fetched_before,
            usize::from(fetches),
            "{label}"
        );
        gh.run_git(&request)
            .unwrap_or_else(|error| panic!("repeated selected checkout: {error:?}"));
        assert_eq!(host_git(&target, &["rev-parse", "refs/agh/selected"]), oid);
        let partial_objects =
            host_git(&target, &["rev-list", "--objects", &oid, "--missing=print"]);
        assert!(
            partial_objects
                .lines()
                .any(|line| line == format!("?{historical_blob}"))
        );
        if label == "cold" {
            let full = world.path().join("full-from-selected.git");
            host_git(
                world.path(),
                &["init", "--quiet", "--bare", full.to_str().unwrap()],
            );
            let prior_refs = host_git(&target, &["for-each-ref"]);
            let prior_config = std::fs::read(target.join("config")).unwrap();
            gh.run_git(&GitRun::fetch_ref(
                &slug,
                &full,
                &oid,
                "refs/agh/source",
                true,
                agh::git::FetchScope::FullSource {
                    cache: target.clone(),
                },
            ))
            .unwrap();
            assert_eq!(
                host_git(&full, &["show", &format!("{historical_commit}:payload")]),
                "historical blob outside the selected tree"
            );
            host_git(
                &full,
                &["fsck", "--full", "--strict", "--no-dangling", &oid],
            );
            assert_eq!(host_git(&target, &["for-each-ref"]), prior_refs);
            assert_eq!(std::fs::read(target.join("config")).unwrap(), prior_config);
            assert_eq!(
                host_git(&target, &["rev-list", "--objects", &oid, "--missing=print"]),
                partial_objects
            );
            let prior_ref = std::fs::read(target.join("refs/agh/selected")).unwrap();
            assert!(!target.join("packed-refs").exists());
            let index = std::fs::read_dir(target.join("objects/pack"))
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .find(|path| path.extension().is_some_and(|extension| extension == "idx"))
                .unwrap();
            let mut corrupt = std::fs::read(&index).unwrap();
            corrupt[0] ^= 1;
            let permissions = std::fs::metadata(&index).unwrap().permissions();
            std::fs::set_permissions(
                &index,
                std::fs::Permissions::from_mode(permissions.mode() | 0o200),
            )
            .unwrap();
            std::fs::write(&index, &corrupt).unwrap();
            std::fs::set_permissions(&index, permissions).unwrap();
            let refused = GitRun::fetch_ref(
                &slug,
                &target,
                &oid,
                "refs/agh/corrupt",
                true,
                agh::git::FetchScope::SelectedCheckout { cache: None },
            );
            assert!(gh.run_git(&refused).is_err());
            assert_eq!(
                std::fs::read(target.join("refs/agh/selected")).unwrap(),
                prior_ref
            );
            assert!(!target.join("refs/agh/corrupt").exists());
            assert!(!target.join("packed-refs").exists());
        }
    }
}

/// A metadata-only preseed and a repeated selected snapshot both reuse verified packs.
#[test]
fn checkout_closure_reuses_verified_metadata_and_completed_snapshot_packs() {
    let world = KeptWorld(Some(World::canonical("git-checkout-pack-reuse")));
    let bare = bare_repo(world.path(), "app");
    let work = world.path().join("app-work");
    std::fs::write(work.join("payload"), b"selected checkout body\n").unwrap();
    host_git(&work, &["add", "payload"]);
    host_git(&work, &["commit", "--quiet", "-m", "selected body"]);
    host_git(&work, &["push", "--quiet", &bare.to_string_lossy(), "main"]);
    host_git(&bare, &["config", "uploadpack.allowFilter", "true"]);
    let target = world.path().join("target.git");
    host_git(
        world.path(),
        &["init", "--quiet", "--bare", target.to_str().unwrap()],
    );
    let slug = common::slug("fixture-dev/app");
    let gh = world
        .client(git_env())
        .git_remote_for_tests("fixture-dev/app", &bare)
        .build();
    gh.run_git(&GitRun::fetch_main(&slug, &target, "refs/agh/metadata").without_blobs())
        .unwrap();
    let oid = host_git(&bare, &["rev-parse", "main"]);
    let request = GitRun::fetch_ref(
        &slug,
        &target,
        &oid,
        "refs/agh/selected",
        true,
        agh::git::FetchScope::SelectedCheckout { cache: None },
    );
    for _ in 0..2 {
        gh.run_git(&request).unwrap_or_else(|error| {
            panic!("verified metadata/checkout pack reuse must succeed: {error:?}")
        });
        assert_eq!(host_git(&target, &["rev-parse", "refs/agh/selected"]), oid);
        let objects = host_git(&target, &["rev-list", "--objects", &oid, "--missing=print"]);
        assert!(
            !objects.lines().any(|line| line.starts_with('?')),
            "{objects}"
        );
    }
}

/// Inject real filesystem races after actual Git verification. No failed
/// installation may publish a ref or overwrite an existing entry.
#[test]
fn checkout_closure_installer_checks_collisions_and_verification_bookends() {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};
    use std::sync::{Mutex, OnceLock, Weak};
    for fault in [
        "identical",
        "different",
        "corrupt",
        "symlink",
        "directory",
        "replace-directory",
        "after-verify",
    ] {
        let world = KeptWorld(Some(World::canonical("git-checkout-installer-races")));
        let bare = bare_repo(world.path(), "app");
        let work = world.path().join("app-work");
        std::fs::write(work.join("payload"), b"required checkout body\n").unwrap();
        host_git(&work, &["add", "payload"]);
        host_git(&work, &["commit", "--quiet", "-m", "required body"]);
        host_git(&work, &["push", "--quiet", &bare.to_string_lossy(), "main"]);
        host_git(&bare, &["config", "uploadpack.allowFilter", "true"]);
        let unrelated = bare_repo(world.path(), "unrelated");
        let unrelated_work = world.path().join("unrelated-work");
        std::fs::write(unrelated_work.join("payload"), b"different valid body\n").unwrap();
        host_git(&unrelated_work, &["add", "payload"]);
        host_git(
            &unrelated_work,
            &["commit", "--quiet", "-m", "different body"],
        );
        host_git(
            &unrelated_work,
            &["push", "--quiet", &unrelated.to_string_lossy(), "main"],
        );
        host_git(&unrelated, &["repack", "-ad"]);
        let unrelated_pack = std::fs::read_dir(unrelated.join("objects/pack"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.extension().is_some_and(|ext| ext == "pack"))
            .unwrap();
        let target = world.path().join("target.git");
        host_git(
            world.path(),
            &["init", "--quiet", "--bare", target.to_str().unwrap()],
        );
        let pack_directory = std::fs::canonicalize(target.join("objects/pack")).unwrap();
        let moved = world.path().join("original-pack-directory");
        let victim = world.path().join("victim");
        std::fs::create_dir(&victim).unwrap();
        let before = host_git(&target, &["for-each-ref"]);
        let effects: Arc<OnceLock<Weak<Fx>>> = Arc::new(OnceLock::new());
        let observed = Arc::clone(&effects);
        let reached = Arc::new(Mutex::new(false));
        let happened = Arc::clone(&reached);
        let originals = Arc::new(Mutex::new(Vec::<(PathBuf, Vec<u8>, u64)>::new()));
        let retained = Arc::clone(&originals);
        let destination_directory = pack_directory.clone();
        let replaced = moved.clone();
        let external = victim.clone();
        let fx = Arc::new(
            Fx::simulated(live_start()).with_git_child_observer_for_tests(move |span| {
                let fx = observed.get().and_then(Weak::upgrade).unwrap();
                let specs = fx.children_run();
                let spec = &specs[span.ordinal];
                if !spec.args.iter().any(|arg| arg == "index-pack")
                    || !spec.args.iter().any(|arg| arg == "--verify")
                {
                    return;
                }
                let verified = Path::new(spec.args.last().unwrap());
                let installed = verified.parent() == Some(destination_directory.as_path());
                if installed != (fault == "after-verify") || *happened.lock().unwrap() {
                    return;
                }
                *happened.lock().unwrap() = true;
                if fault == "replace-directory" {
                    std::fs::rename(&destination_directory, &replaced).unwrap();
                    std::fs::create_dir(&destination_directory).unwrap();
                    std::fs::write(
                        destination_directory.join("victim"),
                        b"untouched replacement\n",
                    )
                    .unwrap();
                    return;
                }
                if fault == "after-verify" {
                    std::fs::set_permissions(verified, std::fs::Permissions::from_mode(0o600))
                        .unwrap();
                    std::fs::write(verified, b"mutated after actual installed verification\n")
                        .unwrap();
                    return;
                }
                for extension in ["pack", "idx"] {
                    let generated = verified.with_extension(extension);
                    let destination = destination_directory.join(generated.file_name().unwrap());
                    if fault == "directory" {
                        std::fs::create_dir(destination).unwrap();
                        continue;
                    }
                    let source = if fault == "different" {
                        unrelated_pack.with_extension(extension)
                    } else {
                        generated
                    };
                    let bytes = if fault == "corrupt" {
                        b"corrupt expected-name entry\n".to_vec()
                    } else {
                        std::fs::read(&source).unwrap()
                    };
                    let actual = if fault == "symlink" {
                        let actual = external.join(extension);
                        std::fs::write(&actual, &bytes).unwrap();
                        symlink(&actual, destination).unwrap();
                        actual
                    } else {
                        std::fs::write(&destination, &bytes).unwrap();
                        destination
                    };
                    let inode = std::fs::symlink_metadata(&actual).unwrap().ino();
                    retained.lock().unwrap().push((actual, bytes, inode));
                }
            }),
        );
        assert!(effects.set(Arc::downgrade(&fx)).is_ok());
        let gh = world
            .client(git_env())
            .git_remote_for_tests("fixture-dev/app", &bare)
            .fx(fx)
            .build();
        let oid = host_git(&bare, &["rev-parse", "main"]);
        let result = gh.run_git(&GitRun::fetch_ref(
            &common::slug("fixture-dev/app"),
            &target,
            &oid,
            "refs/agh/selected",
            true,
            agh::git::FetchScope::SelectedCheckout { cache: None },
        ));
        assert!(
            *reached.lock().unwrap(),
            "{fault}: production verification reached"
        );
        if fault == "identical" {
            result.unwrap_or_else(|error| {
                panic!("identical concurrent install must succeed: {error:?}")
            });
            assert_eq!(host_git(&target, &["rev-parse", "refs/agh/selected"]), oid);
            let checks = gh
                .fx()
                .children_run()
                .iter()
                .filter(|spec| {
                    spec.args.iter().any(|arg| arg == "index-pack")
                        && spec.args.last().is_some_and(|path| {
                            Path::new(path).parent() == Some(pack_directory.as_path())
                        })
                })
                .count();
            assert!(
                checks >= 2,
                "actual installed metadata and blob pairs are independently verified"
            );
        } else {
            let error = result.expect_err("unsafe checkout installation refuses");
            let expected = match fault {
                "different" | "corrupt" => "differs from generated bytes",
                "symlink" | "directory" => "must be physical",
                "replace-directory" => "directory identity changed",
                "after-verify" => "changed during comparison",
                _ => unreachable!(),
            };
            assert!(error.why.contains(expected), "{fault}: {error:?}");
            assert_eq!(
                host_git(&target, &["for-each-ref"]),
                before,
                "{fault}: no ref publication"
            );
        }
        for (path, bytes, inode) in originals.lock().unwrap().iter() {
            assert_eq!(
                &std::fs::read(path).unwrap(),
                bytes,
                "existing entry bytes preserved"
            );
            assert_eq!(
                std::fs::symlink_metadata(path).unwrap().ino(),
                *inode,
                "existing identity preserved"
            );
        }
        if fault == "directory" {
            for entry in std::fs::read_dir(&pack_directory).unwrap() {
                let path = entry.unwrap().path();
                if path
                    .extension()
                    .is_some_and(|ext| ext == "pack" || ext == "idx")
                {
                    assert!(path.is_dir());
                    assert_eq!(std::fs::read_dir(path).unwrap().count(), 0);
                }
            }
        } else if fault == "replace-directory" {
            assert_eq!(
                std::fs::read(pack_directory.join("victim")).unwrap(),
                b"untouched replacement\n"
            );
            assert_eq!(std::fs::read_dir(pack_directory).unwrap().count(), 1);
        }
    }
}

/// Ordinary main and exact fetches produce a real quarantined pack before injection.
/// Verification failure and a replaced original pack directory must both refuse
/// before any cache ref write.
#[test]
fn object_intake_refuses_corrupt_packs_and_replaced_original_directories() {
    use std::sync::{Mutex, OnceLock, Weak};
    for (replace_directory, main) in [false, true]
        .into_iter()
        .flat_map(|replace_directory| [(replace_directory, false), (replace_directory, true)])
    {
        let world = KeptWorld(Some(World::canonical("git-object-intake-refusal")));
        let bare = bare_repo(world.path(), "app");
        let target = world.path().join("target.git");
        host_git(
            world.path(),
            &["init", "--quiet", "--bare", target.to_str().unwrap()],
        );
        let before = host_git(&target, &["for-each-ref"]);
        let effects: Arc<OnceLock<Weak<Fx>>> = Arc::new(OnceLock::new());
        let observed = Arc::clone(&effects);
        let reached = Arc::new(Mutex::new(false));
        let happened = Arc::clone(&reached);
        let original_pack = std::fs::canonicalize(target.join("objects/pack")).unwrap();
        let pack_directory = original_pack.clone();
        let moved = world.path().join("original-pack-directory");
        let fx = Arc::new(
            Fx::simulated(live_start()).with_git_child_observer_for_tests(move |span| {
                let fx = observed.get().and_then(Weak::upgrade).unwrap();
                let specs = fx.children_run();
                let spec = &specs[span.ordinal];
                if !spec.args.iter().any(|arg| arg == "--local") {
                    return;
                }
                assert!(spec.args.iter().any(|arg| arg == "pack-objects"));
                let prefix = Path::new(spec.args.last().unwrap());
                let pack_parent = prefix.parent().and_then(Path::parent).unwrap();
                assert_eq!(
                    std::fs::canonicalize(pack_parent).unwrap(),
                    pack_directory,
                    "the generated pack must be quarantined beneath the actual pack directory"
                );
                *happened.lock().unwrap() = true;
                if replace_directory {
                    std::fs::rename(&pack_directory, &moved).unwrap();
                    std::fs::create_dir(&pack_directory).unwrap();
                    std::fs::write(pack_directory.join("victim"), b"untouched replacement\n")
                        .unwrap();
                } else {
                    let pack = std::fs::read_dir(prefix.parent().unwrap())
                        .unwrap()
                        .map(|entry| entry.unwrap().path())
                        .find(|path| {
                            path.extension()
                                .is_some_and(|extension| extension == "pack")
                        })
                        .unwrap();
                    use std::os::unix::fs::PermissionsExt as _;
                    std::fs::set_permissions(&pack, std::fs::Permissions::from_mode(0o600))
                        .unwrap();
                    std::fs::write(pack, b"invalid real generated pack\n").unwrap();
                }
            }),
        );
        assert!(effects.set(Arc::downgrade(&fx)).is_ok());
        let gh = world
            .client(git_env())
            .git_remote_for_tests("fixture-dev/app", &bare)
            .fx(fx)
            .build();
        let slug = common::slug("fixture-dev/app");
        let request = if main {
            GitRun::fetch_main(&slug, &target, "refs/agh/requested")
        } else {
            GitRun::fetch_ref(
                &slug,
                &target,
                "refs/heads/main",
                "refs/agh/requested",
                true,
                agh::git::FetchScope::NativeExact,
            )
        };
        let error = gh.run_git(&request).expect_err("unsafe intake refuses");
        assert!(
            *reached.lock().unwrap(),
            "the actual pack phase was reached"
        );
        assert_eq!(host_git(&target, &["for-each-ref"]), before);
        if replace_directory {
            assert!(
                error.why.contains("directory identity changed"),
                "{error:?}"
            );
            assert_eq!(
                std::fs::read(original_pack.join("victim")).unwrap(),
                b"untouched replacement\n"
            );
            assert_eq!(std::fs::read_dir(&original_pack).unwrap().count(), 1);
        } else {
            assert!(
                error.next.starts_with("object intake step: verify;"),
                "{error:?}"
            );
        }
    }
}

#[test]
fn object_intake_refuses_a_different_valid_pack_under_the_expected_hash_name() {
    use std::sync::{Mutex, OnceLock, Weak};
    for different in [false, true] {
        let world = KeptWorld(Some(World::canonical("git-pack-name-collision")));
        let bare = bare_repo(world.path(), "app");
        let unrelated = bare_repo(world.path(), "unrelated");
        host_git(&unrelated, &["repack", "-ad"]);
        let unrelated_pack = std::fs::read_dir(unrelated.join("objects/pack"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "pack")
            })
            .unwrap();
        let work = world.path().join("app-work");
        std::fs::write(
            work.join("payload"),
            b"a required body absent from the target\n",
        )
        .unwrap();
        host_git(&work, &["add", "payload"]);
        host_git(&work, &["commit", "--quiet", "-m", "payload"]);
        host_git(&work, &["push", "--quiet", &bare.to_string_lossy(), "main"]);
        host_git(&bare, &["config", "uploadpack.allowFilter", "true"]);
        let target = world.path().join("target.git");
        host_git(
            world.path(),
            &["init", "--quiet", "--bare", target.to_str().unwrap()],
        );
        let slug = common::slug("fixture-dev/app");
        world
            .client(git_env())
            .git_remote_for_tests("fixture-dev/app", &bare)
            .build()
            .run_git(&GitRun::fetch_main(&slug, &target, "refs/agh/metadata").without_blobs())
            .unwrap();
        let before = host_git(&target, &["for-each-ref"]);
        let requested = host_git(&bare, &["rev-parse", "main"]);
        let blob = host_git(&bare, &["rev-parse", "main:payload"]);
        assert!(
            host_git(
                &target,
                &["rev-list", "--objects", "--all", "--missing=print"]
            )
            .contains(&format!("?{blob}"))
        );
        let effects: Arc<OnceLock<Weak<Fx>>> = Arc::new(OnceLock::new());
        let observed = Arc::clone(&effects);
        let reached = Arc::new(Mutex::new(false));
        let happened = Arc::clone(&reached);
        let pack_directory = target.join("objects/pack");
        let fx = Arc::new(
            Fx::simulated(live_start()).with_git_child_observer_for_tests(move |span| {
                let fx = observed.get().and_then(Weak::upgrade).unwrap();
                let specs = fx.children_run();
                let spec = &specs[span.ordinal];
                if !spec.args.iter().any(|arg| arg == "--local") {
                    return;
                }
                let prefix = Path::new(spec.args.last().unwrap());
                let generated = std::fs::read_dir(prefix.parent().unwrap())
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .find(|path| {
                        path.extension()
                            .is_some_and(|extension| extension == "pack")
                    })
                    .unwrap();
                for extension in ["pack", "idx"] {
                    let name = generated
                        .with_extension(extension)
                        .file_name()
                        .unwrap()
                        .to_owned();
                    let source = if different {
                        &unrelated_pack
                    } else {
                        &generated
                    };
                    std::fs::copy(source.with_extension(extension), pack_directory.join(name))
                        .unwrap();
                }
                *happened.lock().unwrap() = true;
            }),
        );
        assert!(effects.set(Arc::downgrade(&fx)).is_ok());
        let gh = world
            .client(git_env())
            .git_remote_for_tests("fixture-dev/app", &bare)
            .fx(fx)
            .build();
        let result = gh.run_git(&GitRun::fetch_ref(
            &slug,
            &target,
            &requested,
            "refs/agh/requested",
            true,
            agh::git::FetchScope::NativeExact,
        ));
        assert!(
            *reached.lock().unwrap(),
            "the actual expected-name collision was installed"
        );
        if different {
            assert!(
                result.is_err(),
                "a valid DIFFERENT pack pair must not substitute for the generated pack"
            );
            assert_eq!(host_git(&target, &["for-each-ref"]), before);
        } else {
            result
                .unwrap_or_else(|error| panic!("an identical valid pair may be reused: {error:?}"));
            assert_eq!(host_git(&target, &["cat-file", "-t", &blob]), "blob");
        }
    }
}

/// An unrelated incomplete ref must not make a complete main snapshot cold.
/// An incomplete snapshot itself must still fetch every missing object.
#[test]
fn main_lends_only_its_complete_snapshot_despite_an_unrelated_partial_ref() {
    use std::sync::{Mutex, OnceLock, Weak};
    for complete in [true, false] {
        let world = World::canonical("main-snapshot-have");
        let bare = bare_repo(world.path(), "app");
        let work = world.path().join("app-work");
        std::fs::write(work.join("payload"), b"required main snapshot body\n").unwrap();
        host_git(&work, &["add", "payload"]);
        host_git(&work, &["commit", "--quiet", "-m", "main payload"]);
        host_git(&work, &["push", "--quiet", bare.to_str().unwrap(), "main"]);
        host_git(&bare, &["config", "uploadpack.allowFilter", "true"]);
        let target = world.path().join("target.git");
        host_git(
            world.path(),
            &["init", "--quiet", "--bare", target.to_str().unwrap()],
        );
        let slug = common::slug("fixture-dev/app");
        let gh = world
            .client(git_env())
            .git_remote_for_tests("fixture-dev/app", &bare)
            .build();
        let request = GitRun::fetch_main(&slug, &target, "refs/agh/main");
        gh.run_git(&if complete {
            request
        } else {
            request.without_blobs()
        })
        .unwrap();
        let snapshot = host_git(&bare, &["rev-parse", "main"]);
        std::fs::write(
            work.join("unrelated"),
            b"absent from the complete snapshot\n",
        )
        .unwrap();
        host_git(&work, &["add", "unrelated"]);
        host_git(
            &work,
            &["commit", "--quiet", "-m", "unrelated partial branch"],
        );
        host_git(
            &work,
            &[
                "push",
                "--quiet",
                bare.to_str().unwrap(),
                "HEAD:refs/heads/partial",
            ],
        );
        let partial = host_git(&work, &["rev-parse", "HEAD"]);
        // The metadata main operation produces the same unrelated incomplete closure.
        host_git(&bare, &["update-ref", "refs/heads/main", &partial]);
        gh.run_git(&GitRun::fetch_main(&slug, &target, "refs/agh/partial").without_blobs())
            .unwrap();
        host_git(&bare, &["update-ref", "refs/heads/main", &snapshot]);
        assert!(
            host_git(
                &target,
                &["rev-list", "--objects", "--all", "--missing=print"]
            )
            .lines()
            .any(|line| line.starts_with('?'))
        );
        let effects = Arc::new(OnceLock::<Weak<Fx>>::new());
        let observed = Arc::clone(&effects);
        let counts = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&counts);
        let fx = Arc::new(
            Fx::simulated(live_start()).with_git_child_observer_for_tests(move |span| {
                let fx = observed.get().and_then(Weak::upgrade).unwrap();
                let specs = fx.children_run();
                let spec = &specs[span.ordinal];
                if spec.args.iter().any(|arg| arg == "fetch") {
                    let scratch = spec.cwd.as_ref().unwrap();
                    assert!(
                        host_git(
                            scratch,
                            &["rev-list", "--alternate-refs", "--not", "refs/agh/source"]
                        )
                        .is_empty(),
                        "the borrowed store's other refs must not become negotiation haves"
                    );
                    let count = host_git(scratch, &["count-objects", "-v"]);
                    let received = count
                        .lines()
                        .filter_map(|line| {
                            let (key, value) = line.split_once(": ")?;
                            matches!(key, "count" | "in-pack")
                                .then(|| value.parse::<usize>().unwrap())
                        })
                        .sum::<usize>();
                    recorded.lock().unwrap().push(received > 0);
                }
            }),
        );
        assert!(effects.set(Arc::downgrade(&fx)).is_ok());
        let gh = world
            .client(git_env())
            .git_remote_for_tests("fixture-dev/app", &bare)
            .fx(fx)
            .build();
        gh.run_git(&GitRun::fetch_main(&slug, &target, "refs/agh/main"))
            .unwrap();
        let notes = gh.fx().retry_notes();
        assert_eq!(
            notes
                .iter()
                .any(|note| note.contains("snapshot refs/agh/main is incomplete")),
            !complete
        );
        assert_eq!(
            *counts.lock().unwrap(),
            vec![!complete],
            "a complete snapshot downloads no objects; an incomplete one must start cold"
        );
        assert!(
            !host_git(
                &target,
                &["rev-list", "--objects", "refs/agh/main", "--missing=print"]
            )
            .lines()
            .any(|line| line.starts_with('?'))
        );
        assert_eq!(
            host_git(&target, &["rev-parse", "refs/agh/partial"]),
            partial
        );
    }
}

/// An exact batch's requested graph is complete even when another local ref is not.
/// The private ref must still be published, without transferring or importing the cached data.
#[test]
fn literal_batch_reuses_complete_roots_despite_an_unrelated_partial_ref() {
    let world = KeptWorld(Some(World::canonical("batch-local-complete")));
    let bare = bare_repo(world.path(), "app");
    let work = world.path().join("app-work");
    std::fs::write(work.join("payload"), b"complete requested bytes\n").unwrap();
    host_git(&work, &["add", "payload"]);
    host_git(&work, &["commit", "--quiet", "-m", "complete root"]);
    host_git(&work, &["push", "--quiet", bare.to_str().unwrap(), "main"]);
    let oid = host_git(&bare, &["rev-parse", "main"]);
    let target = world.path().join("target.git");
    host_git(
        world.path(),
        &[
            "clone",
            "--quiet",
            "--bare",
            "--no-hardlinks",
            bare.to_str().unwrap(),
            target.to_str().unwrap(),
        ],
    );
    std::fs::write(work.join("unrelated"), b"absent unrelated tree\n").unwrap();
    host_git(&work, &["add", "unrelated"]);
    host_git(&work, &["commit", "--quiet", "-m", "unrelated partial"]);
    let partial = host_git(&work, &["rev-parse", "HEAD"]);
    let partial_tree = host_git(&work, &["rev-parse", "HEAD^{tree}"]);
    let raw = super::raw_blob_tests::git_input(&work, &["cat-file", "commit", &partial], None);
    assert!(raw.status.success());
    let inserted = super::raw_blob_tests::git_input(
        &target,
        &["hash-object", "-t", "commit", "-w", "--stdin"],
        Some(&raw.stdout),
    );
    assert!(inserted.status.success());
    host_git(&target, &["update-ref", "refs/agh/partial", &partial]);
    assert!(
        !super::raw_blob_tests::git_input(&target, &["cat-file", "-t", &partial_tree], None)
            .status
            .success()
    );
    assert!(
        host_git(&target, &["rev-list", "--objects", &oid, "--missing=print"])
            .lines()
            .all(|line| !line.starts_with('?'))
    );
    let head = std::fs::read(target.join("HEAD")).unwrap();
    let config = std::fs::read(target.join("config")).unwrap();
    std::fs::write(target.join("FETCH_HEAD"), b"keep fetch head\n").unwrap();
    let gh = world
        .client(git_env())
        .git_remote_for_tests("fixture-dev/app", &bare)
        .build();
    gh.run_git(&GitRun::fetch_batch(
        &common::slug("fixture-dev/app"),
        &target,
        fetch_rows(&[(&oid, "refs/agh/pinned", &oid)]),
        true,
    ))
    .unwrap();
    assert_eq!(
        host_git(
            &target,
            &["for-each-ref", "--format=%(refname) %(objectname)"]
        ),
        format!("refs/agh/partial {partial}\nrefs/agh/pinned {oid}\nrefs/heads/main {oid}")
    );
    assert_eq!(
        host_git(&target, &["show", "refs/agh/pinned:payload"]),
        "complete requested bytes"
    );
    let data_steps: Vec<_> = gh
        .fx()
        .children_run()
        .into_iter()
        .filter(|spec| {
            spec.args.iter().any(|arg| {
                arg == "init" || arg == "fetch" || arg == "pack-objects" || arg == "index-pack"
            })
        })
        .collect();
    assert!(
        data_steps.is_empty(),
        "complete local roots need no transfer or import: {data_steps:?}"
    );
    assert_eq!(std::fs::read(target.join("HEAD")).unwrap(), head);
    assert_eq!(std::fs::read(target.join("config")).unwrap(), config);
    assert_eq!(
        std::fs::read(target.join("FETCH_HEAD")).unwrap(),
        b"keep fetch head\n"
    );
    assert!(!target.join("objects/info/alternates").exists());
    assert!(
        !super::raw_blob_tests::git_input(&target, &["cat-file", "-t", &partial_tree], None)
            .status
            .success(),
        "an unrelated incomplete ref is neither requested nor hydrated"
    );
}

fn historical_batch_source() -> (KeptWorld, PathBuf, String, String, String) {
    let world = KeptWorld(Some(World::canonical("batch-local-history")));
    let bare = bare_repo(world.path(), "app");
    let work = world.path().join("app-work");
    let base = host_git(&bare, &["rev-parse", "main"]);
    std::fs::write(work.join("payload"), b"required historical body\n").unwrap();
    host_git(&work, &["add", "payload"]);
    host_git(&work, &["commit", "--quiet", "-m", "historical body"]);
    let historical = host_git(&work, &["rev-parse", "HEAD:payload"]);
    std::fs::write(work.join("payload"), b"selected checkout body\n").unwrap();
    host_git(&work, &["commit", "--quiet", "-am", "selected body"]);
    host_git(&work, &["push", "--quiet", bare.to_str().unwrap(), "main"]);
    host_git(&bare, &["config", "uploadpack.allowFilter", "true"]);
    let oid = host_git(&bare, &["rev-parse", "main"]);
    (world, bare, base, oid, historical)
}

/// A present commit and selected checkout do not prove historical closure. Every requested
/// root must be complete before a literal batch can reuse local objects without transfer.
#[test]
fn literal_batch_completes_missing_historical_bodies_and_every_requested_root() {
    let (world, bare, base, oid, historical) = historical_batch_source();
    let slug = common::slug("fixture-dev/app");
    for mixed in [false, true] {
        let target = world.path().join(format!("target-{mixed}.git"));
        host_git(
            world.path(),
            &["init", "--quiet", "--bare", target.to_str().unwrap()],
        );
        let gh = world
            .client(git_env())
            .git_remote_for_tests("fixture-dev/app", &bare)
            .build();
        gh.run_git(&GitRun::fetch_ref(
            &slug,
            &target,
            &oid,
            "refs/agh/selected",
            true,
            agh::git::FetchScope::SelectedCheckout { cache: None },
        ))
        .unwrap();
        assert_eq!(
            host_git(&target, &["show", "refs/agh/selected:payload"]),
            "selected checkout body"
        );
        assert!(
            host_git(&target, &["rev-list", "--objects", &oid, "--missing=print"])
                .lines()
                .any(|line| line == format!("?{historical}"))
        );
        assert!(
            host_git(
                &target,
                &["rev-list", "--objects", &base, "--missing=print"]
            )
            .lines()
            .all(|line| !line.starts_with('?')),
            "the first mixed root is complete"
        );
        let before = gh.fx().children_run().len();
        let rows = if mixed {
            fetch_rows(&[
                (&base, "refs/agh/base", &base),
                (&oid, "refs/agh/pinned", &oid),
            ])
        } else {
            fetch_rows(&[(&oid, "refs/agh/pinned", &oid)])
        };
        gh.run_git(&GitRun::fetch_batch(&slug, &target, rows, true))
            .unwrap();
        assert_eq!(host_git(&target, &["rev-parse", "refs/agh/pinned"]), oid);
        if mixed {
            assert_eq!(host_git(&target, &["rev-parse", "refs/agh/base"]), base);
        }
        assert_eq!(
            host_git(&target, &["cat-file", "blob", &historical]),
            "required historical body"
        );
        assert!(
            host_git(&target, &["rev-list", "--objects", &oid, "--missing=print"])
                .lines()
                .all(|line| !line.starts_with('?'))
        );
        assert!(
            gh.fx()
                .children_run()
                .iter()
                .skip(before)
                .any(|spec| spec.args.iter().any(|arg| arg == "fetch")),
            "an incomplete requested closure must use normal transfer"
        );
    }
}

/// A shallow boundary introduced after type inventory cannot hide a missing ancestor's body
/// from the full-closure proof. Either acquisition refuses before publication or completes it.
#[test]
fn a_shallow_boundary_created_after_type_inventory_cannot_hide_missing_history() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{OnceLock, Weak};

    let (world, bare, _, oid, historical) = historical_batch_source();
    let target = world.path().join("shallow-race.git");
    host_git(
        world.path(),
        &["init", "--quiet", "--bare", target.to_str().unwrap()],
    );
    let slug = common::slug("fixture-dev/app");
    let seed = world
        .client(git_env())
        .git_remote_for_tests("fixture-dev/app", &bare)
        .build();
    seed.run_git(&GitRun::fetch_ref(
        &slug,
        &target,
        &oid,
        "refs/agh/selected",
        true,
        agh::git::FetchScope::SelectedCheckout { cache: None },
    ))
    .unwrap();
    assert!(!target.join("shallow").exists());
    assert!(
        !super::raw_blob_tests::git_input(&target, &["cat-file", "-t", &historical], None)
            .status
            .success(),
        "the original historical body is actually absent"
    );
    let shallow = target.join("shallow");
    let boundary = format!("{oid}\n");
    let changed = shallow.clone();
    let bytes = boundary.clone();
    let injected = Arc::new(AtomicBool::new(false));
    let happened = Arc::clone(&injected);
    let effects = Arc::new(OnceLock::<Weak<Fx>>::new());
    let observed = Arc::clone(&effects);
    let fx = Arc::new(
        Fx::simulated(live_start()).with_git_child_observer_for_tests(move |span| {
            let fx = observed.get().and_then(Weak::upgrade).unwrap();
            let specs = fx.children_run();
            if specs[span.ordinal]
                .args
                .iter()
                .any(|arg| arg == "--batch-check=%(objectname) %(objecttype)")
                && !happened.swap(true, Ordering::SeqCst)
            {
                assert_eq!(span.code, Some(0), "the type inventory completed");
                std::fs::write(&changed, &bytes).unwrap();
            }
        }),
    );
    assert!(effects.set(Arc::downgrade(&fx)).is_ok());
    let gh = world
        .client(git_env())
        .git_remote_for_tests("fixture-dev/app", &bare)
        .fx(fx)
        .build();
    let result = gh.run_git(&GitRun::fetch_batch(
        &slug,
        &target,
        fetch_rows(&[(&oid, "refs/agh/pinned", &oid)]),
        true,
    ));
    assert!(
        injected.load(Ordering::SeqCst),
        "the boundary race was exercised"
    );
    assert_eq!(std::fs::read(&shallow).unwrap(), boundary.as_bytes());
    if result.is_ok() {
        assert_eq!(host_git(&target, &["rev-parse", "refs/agh/pinned"]), oid);
        assert_eq!(
            host_git(&target, &["cat-file", "blob", &historical]),
            "required historical body"
        );
        assert!(
            gh.fx()
                .children_run()
                .iter()
                .any(|spec| spec.args.iter().any(|arg| arg == "fetch")),
            "the incomplete original graph needs a real transfer"
        );
    } else {
        assert!(
            !super::raw_blob_tests::git_input(
                &target,
                &["rev-parse", "--verify", "refs/agh/pinned"],
                None
            )
            .status
            .success(),
            "a refused acquisition publishes no pin"
        );
    }
}
