// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use super::*;

pub(crate) fn store_bytes(root: &Path) -> Vec<(PathBuf, Option<Vec<u8>>)> {
    let mut pending = vec![root.to_path_buf()];
    let mut entries = Vec::new();
    while let Some(path) = pending.pop() {
        let meta = std::fs::symlink_metadata(&path).unwrap();
        let bytes = if meta.is_dir() {
            pending.extend(
                std::fs::read_dir(&path)
                    .unwrap()
                    .map(|entry| entry.unwrap().path()),
            );
            None
        } else {
            assert!(meta.is_file());
            Some(std::fs::read(&path).unwrap())
        };
        entries.push((path.strip_prefix(root).unwrap().to_path_buf(), bytes));
    }
    entries.sort_unstable();
    entries
}

#[test]
fn full_source_readonly_cache_refuses_physical_receiver_aliases() {
    use std::os::unix::fs::MetadataExt as _;

    let world = World::canonical("full-source-cache-aliases");
    let remote = bare_repo(world.path(), "alias-remote");
    let oid = host_git(&remote, &["rev-parse", "refs/heads/main"]);
    let same = world.path().join("same.git");
    host_git(
        world.path(),
        &[
            "clone",
            "-q",
            "--bare",
            "--no-hardlinks",
            remote.to_str().unwrap(),
            same.to_str().unwrap(),
        ],
    );
    let shared = world.path().join("shared");
    host_git(
        world.path(),
        &[
            "clone",
            "-q",
            "--no-hardlinks",
            remote.to_str().unwrap(),
            shared.to_str().unwrap(),
        ],
    );
    let cache_worktree = world.path().join("cache-worktree");
    let receiver_worktree = world.path().join("receiver-worktree");
    for worktree in [&cache_worktree, &receiver_worktree] {
        host_git(
            &shared,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                worktree.to_str().unwrap(),
                &oid,
            ],
        );
    }
    assert_ne!(cache_worktree, receiver_worktree);
    let distinct = world.path().join("distinct.git");
    host_git(
        world.path(),
        &["init", "-q", "--bare", distinct.to_str().unwrap()],
    );

    // A genuine repository inside a cache's object namespace shares no store
    // root identity, but publishing into it still writes the read-only input.
    let nested_cache = world.path().join("nested-cache.git");
    host_git(
        world.path(),
        &[
            "clone",
            "-q",
            "--bare",
            "--no-hardlinks",
            remote.to_str().unwrap(),
            nested_cache.to_str().unwrap(),
        ],
    );
    let nested = nested_cache.join("objects/nested-receiver.git");
    host_git(
        world.path(),
        &["init", "-q", "--bare", nested.to_str().unwrap()],
    );

    for (cache, receiver, aliases, same_common) in [
        (&same, &same, true, true),
        (&cache_worktree, &receiver_worktree, true, true),
        (&nested_cache, &nested, true, false),
        (&same, &distinct, false, false),
    ] {
        let common = |dir: &Path| {
            PathBuf::from(host_git(
                dir,
                &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            ))
        };
        let cache_common = common(cache);
        let receiver_common = common(receiver);
        let cache_file = std::fs::File::open(&cache_common).unwrap();
        let receiver_file = std::fs::File::open(&receiver_common).unwrap();
        let identity = |file: &std::fs::File| {
            let meta = file.metadata().unwrap();
            (meta.dev(), meta.ino())
        };
        assert_eq!(
            identity(&cache_file) == identity(&receiver_file),
            same_common
        );
        let cache_before = store_bytes(&cache_common);
        let receiver_before = store_bytes(&receiver_common);
        let cache_refs = host_git(cache, &["for-each-ref"]);
        let receiver_refs = host_git(receiver, &["for-each-ref"]);
        let gh = world
            .client(git_env())
            .fx(Arc::new(Fx::simulated(live_start())))
            .git_remote_for_tests("fixture-dev/app", &remote)
            .build();
        let result = gh.run_git(&GitRun::fetch_ref(
            &common::slug("fixture-dev/app"),
            receiver,
            &oid,
            "refs/agh/alias-control",
            true,
            agh::git::FetchScope::FullSource {
                cache: cache.to_path_buf(),
            },
        ));
        if aliases {
            let error = result.unwrap_err();
            assert_eq!(error.kind, Kind::Refused);
            assert!(matches!(&error.sub, Sub::GuardFailed { .. }));
            assert_eq!(store_bytes(&receiver_common), receiver_before);
            assert_eq!(host_git(receiver, &["for-each-ref"]), receiver_refs);
            assert!(gh.fx().children_run().iter().all(|spec| {
                !spec
                    .args
                    .iter()
                    .any(|arg| arg == "pack-objects" || arg == "update-ref")
            }));
        } else {
            result.unwrap();
            assert_eq!(
                host_git(receiver, &["rev-parse", "refs/agh/alias-control"]),
                oid
            );
            host_git(
                receiver,
                &["fsck", "--full", "--strict", "--no-dangling", &oid],
            );
        }
        assert_eq!(store_bytes(&cache_common), cache_before);
        assert_eq!(host_git(cache, &["for-each-ref"]), cache_refs);
    }
}

#[test]
fn metadata_only_is_refused_for_exact_ref_before_any_child() {
    let world = World::canonical("metadata-only-exact-ref-refusal");
    let gh = world
        .client(git_env())
        .fx(Arc::new(Fx::simulated(live_start())))
        .build();
    let slug = agh::slug::Slug::new("fixture-dev", "app").unwrap();
    let request = GitRun::fetch_ref(
        &slug,
        Path::new("/not-opened"),
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "refs/agh/exact",
        true,
        agh::git::FetchScope::MetadataOnly,
    );
    assert_eq!(gh.run_git(&request).unwrap_err().kind, Kind::Usage);
    assert!(gh.fx().children_run().is_empty());
}

#[test]
fn complete_and_partial_source_caches_preserve_real_payload_and_native_transfer_results() {
    use std::os::unix::fs::PermissionsExt as _;
    use std::sync::Mutex;

    let world = World::canonical("git-source-cache-coverage");
    let remote = bare_repo(world.path(), "coverage");
    let work = world.path().join("coverage-work");
    std::fs::write(work.join("historical"), b"cache-only historical bytes\n").unwrap();
    host_git(&work, &["add", "."]);
    host_git(&work, &["commit", "-qm", "historical payload"]);
    let middle = host_git(&work, &["rev-parse", "HEAD"]);
    let historical = host_git(&work, &["rev-parse", "HEAD:historical"]);
    host_git(
        &work,
        &["push", "-q", remote.to_str().unwrap(), "HEAD:main"],
    );
    let partial = world.path().join("partial.git");
    host_git(
        world.path(),
        &[
            "clone",
            "-q",
            "--bare",
            "--no-hardlinks",
            remote.to_str().unwrap(),
            partial.to_str().unwrap(),
        ],
    );
    host_git(&work, &["rm", "-q", "historical"]);
    std::fs::write(work.join("current"), b"new remote payload\n").unwrap();
    host_git(&work, &["add", "."]);
    host_git(&work, &["commit", "-qm", "selected payload"]);
    host_git(
        &work,
        &["push", "-q", remote.to_str().unwrap(), "HEAD:main"],
    );
    let target = host_git(&work, &["rev-parse", "HEAD"]);
    let complete = world.path().join("complete.git");
    host_git(
        world.path(),
        &[
            "clone",
            "-q",
            "--bare",
            "--no-hardlinks",
            remote.to_str().unwrap(),
            complete.to_str().unwrap(),
        ],
    );
    host_git(&remote, &["config", "uploadpack.allowFilter", "true"]);
    host_git(
        &remote,
        &["config", "uploadpack.allowAnySHA1InWant", "true"],
    );
    let remote_old = remote
        .join("objects")
        .join(&historical[..2])
        .join(&historical[2..]);
    assert!(remote_old.is_file());
    let remote_old_bytes = std::fs::read(&remote_old).unwrap();
    std::fs::remove_file(&remote_old).unwrap();
    let slug = common::slug("fixture-dev/app");
    let mut complete_request = None;
    for (cache, full) in [(&complete, true), (&partial, false)] {
        if !full {
            // A partial cache cannot repair an incomplete authoritative source.
            // This refusal uses its own receiver; the positive case below starts
            // only after restoring the exact test-owned provider object.
            let refused = world.path().join("partial-incomplete-source-receiver.git");
            host_git(
                world.path(),
                &["init", "-q", "--bare", refused.to_str().unwrap()],
            );
            let cache_before = store_bytes(cache);
            let receiver_before = store_bytes(&refused);
            let gh = world
                .client(git_env())
                .fx(Arc::new(Fx::simulated(live_start())))
                .git_remote_for_tests("fixture-dev/app", &remote)
                .build();
            assert!(
                gh.run_git(&GitRun::fetch_ref(
                    &slug,
                    &refused,
                    &target,
                    "refs/agh/source",
                    true,
                    agh::git::FetchScope::FullSource {
                        cache: cache.to_path_buf()
                    },
                ))
                .is_err()
            );
            assert_eq!(store_bytes(cache), cache_before);
            assert_eq!(store_bytes(&refused), receiver_before);
            assert!(
                !gh.fx().children_run().iter().any(|spec| {
                    spec.args
                        .iter()
                        .any(|arg| arg == "update-ref" || arg == "pack-objects")
                }),
                "incomplete source cannot publish objects or refs"
            );
            std::fs::write(&remote_old, &remote_old_bytes).unwrap();
            host_git(
                &remote,
                &["fsck", "--full", "--strict", "--no-dangling", &target],
            );
        }
        let receiver = world.path().join(if full {
            "complete-receiver.git"
        } else {
            "partial-receiver.git"
        });
        host_git(
            world.path(),
            &["init", "-q", "--bare", receiver.to_str().unwrap()],
        );
        let config = std::fs::read(cache.join("config")).unwrap();
        let refs = host_git(cache, &["show-ref"]);
        let spans = Arc::new(Mutex::new(Vec::new()));
        let observed = spans.clone();
        let fx = Fx::simulated(live_start()).with_git_child_observer_for_tests(move |span| {
            observed.lock().unwrap().push(span);
        });
        let gh = world
            .client(git_env())
            .fx(Arc::new(fx))
            .git_remote_for_tests("fixture-dev/app", &remote)
            .build();
        gh.run_git(&GitRun::fetch_ref(
            &slug,
            &receiver,
            &target,
            "refs/agh/source",
            true,
            agh::git::FetchScope::FullSource {
                cache: cache.to_path_buf(),
            },
        ))
        .unwrap();
        assert_eq!(
            host_git(&receiver, &["show", &format!("{middle}:historical")]),
            "cache-only historical bytes"
        );
        assert_eq!(
            host_git(&receiver, &["show", &format!("{target}:current")]),
            "new remote payload"
        );
        host_git(
            &receiver,
            &["fsck", "--full", "--strict", "--no-dangling", &target],
        );
        assert!(!receiver.join("objects/info/alternates").exists());
        assert!(!receiver.join("shallow").exists());
        assert_eq!(std::fs::read(cache.join("config")).unwrap(), config);
        assert_eq!(host_git(cache, &["show-ref"]), refs);
        let calls = gh.fx().children_run();
        let whole: Vec<_> = calls
            .iter()
            .enumerate()
            .filter(|(_, spec)| spec.args.iter().any(|arg| arg == "--refetch"))
            .collect();
        assert_eq!(whole.len(), usize::from(full));
        if let Some((ordinal, spec)) = whole.first() {
            let spans = spans.lock().unwrap();
            let returned = spans.iter().find(|span| span.ordinal == *ordinal).unwrap();
            assert_eq!((returned.code, returned.error), (Some(0), None));
            assert!(spec.args.iter().any(|arg| arg == target.as_str()));
            complete_request = Some((spec.program.clone(), spec.args.clone()));
        }
        let mixed = calls
            .iter()
            .filter(|spec| {
                spec.args.iter().any(|arg| arg == "fetch")
                    && spec.args.iter().any(|arg| arg == "--stdin")
            })
            .count();
        assert_eq!(
            mixed, 0,
            "cache DATA never requires raw-object upload permission"
        );
        let exact_root = calls
            .iter()
            .filter(|spec| {
                spec.args.iter().any(|arg| arg == "fetch")
                    && spec.args.iter().any(|arg| arg == "--no-filter")
                    && !spec.args.iter().any(|arg| arg == "--refetch")
                    && spec
                        .args
                        .iter()
                        .any(|arg| arg == format!("+{target}:refs/agh/source").as_str())
            })
            .count();
        assert_eq!(
            exact_root,
            usize::from(!full),
            "incomplete cache uses the authoritative full-history root"
        );
    }
    // Replace the receiver while cache hydration is in flight, before the
    // importer captures its own destination view. The original outer directory
    // descriptions must still refuse this same-path substitution.
    let (program, args) = complete_request.unwrap();
    for component in ["objects", "objects/pack"] {
        let receiver = world.path().join(if component == "objects" {
            "replaced-objects-receiver.git"
        } else {
            "replaced-pack-receiver.git"
        });
        host_git(
            world.path(),
            &["init", "-q", "--bare", receiver.to_str().unwrap()],
        );
        let changed = receiver.join(component);
        let preserved = receiver.join("original-directory");
        let source = changed.clone();
        let retained = preserved.clone();
        let fx = Fx::simulated(live_start()).with_git_intercept_once_for_tests(
            program.clone(),
            args.clone(),
            move || {
                std::fs::rename(&source, &retained).unwrap();
                std::fs::create_dir(&source).unwrap();
                if component == "objects" {
                    std::fs::create_dir(source.join("pack")).unwrap();
                }
            },
            |_| {},
        );
        let gh = world
            .client(git_env())
            .fx(Arc::new(fx))
            .git_remote_for_tests("fixture-dev/app", &remote)
            .build();
        let error = gh
            .run_git(&GitRun::fetch_ref(
                &slug,
                &receiver,
                &target,
                "refs/agh/source",
                true,
                agh::git::FetchScope::FullSource {
                    cache: complete.to_path_buf(),
                },
            ))
            .unwrap_err();
        assert!(
            error
                .why
                .contains("object intake directory identity changed"),
            "{error:?}"
        );
        assert!(
            preserved.is_dir(),
            "the actual substitution control must run"
        );
        assert!(changed.is_dir());
        assert!(host_git(&receiver, &["for-each-ref"]).is_empty());
    }
    // Corrupt the actual cache only after all ordered coverage queries. The exact
    // full transfer still runs and its real failure must not enter mixed hydration.
    let receiver = world.path().join("failed-full-receiver.git");
    host_git(
        world.path(),
        &["init", "-q", "--bare", receiver.to_str().unwrap()],
    );
    let old = complete
        .join("objects")
        .join(&historical[..2])
        .join(&historical[2..]);
    assert!(old.is_file());
    let spans = Arc::new(Mutex::new(Vec::new()));
    let observed = spans.clone();
    let fx = Fx::simulated(live_start())
        .with_git_child_observer_for_tests(move |span| {
            observed.lock().unwrap().push(span);
        })
        .with_git_intercept_once_for_tests(
            program,
            args,
            move || {
                std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o600)).unwrap();
                std::fs::write(old, b"corrupt after actual coverage").unwrap();
            },
            |_| {},
        );
    let gh = world
        .client(git_env())
        .fx(Arc::new(fx))
        .git_remote_for_tests("fixture-dev/app", &remote)
        .build();
    assert!(
        gh.run_git(&GitRun::fetch_ref(
            &slug,
            &receiver,
            &target,
            "refs/agh/source",
            true,
            agh::git::FetchScope::FullSource {
                cache: complete.to_path_buf()
            }
        ))
        .is_err()
    );
    assert!(host_git(&receiver, &["for-each-ref"]).is_empty());
    let calls = gh.fx().children_run();
    let (ordinal, _) = calls
        .iter()
        .enumerate()
        .find(|(_, spec)| spec.args.iter().any(|arg| arg == "--refetch"))
        .unwrap();
    assert!(
        spans
            .lock()
            .unwrap()
            .iter()
            .any(|span| span.ordinal == ordinal && span.code != Some(0))
    );
    assert!(
        !calls
            .iter()
            .any(|spec| spec.args.iter().any(|arg| arg == "fetch")
                && spec.args.iter().any(|arg| arg == "--stdin")),
        "failed full transfer cannot fall back"
    );
}
