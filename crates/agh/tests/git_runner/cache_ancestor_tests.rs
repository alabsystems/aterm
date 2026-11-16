// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Real Git pack traffic from a near-current immutable source cache.

use super::*;
use std::os::unix::fs::PermissionsExt as _;

fn quoted(path: &Path) -> String {
    format!("'{}'", path.to_str().unwrap().replace('\'', "'\"'\"'"))
}

#[derive(Clone, Copy, PartialEq)]
enum CacheShape {
    Complete,
    DanglingWanted,
    PrivateHead,
    WantedHead,
    Snapshot,
    MissingSelected,
}

fn check(shape: CacheShape, full: bool) {
    let dangling_wanted = matches!(shape, CacheShape::DanglingWanted | CacheShape::WantedHead);
    let snapshot_only = matches!(shape, CacheShape::Snapshot | CacheShape::MissingSelected);
    let world = World::canonical(if dangling_wanted {
        "source-cache-dangling-wanted"
    } else {
        "source-cache-near-current"
    });
    let remote = bare_repo(world.path(), "delta");
    let work = world.path().join("delta-work");
    let mut random = 0x19ad_0a25_7733_20fe_u64;
    for index in 0..32 {
        let bytes: Vec<_> = (0..4096)
            .map(|_| {
                random = random
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1);
                (random >> 56) as u8
            })
            .collect();
        std::fs::write(work.join(format!("stable-{index}")), bytes).unwrap();
    }
    std::fs::write(
        work.join("retired"),
        b"historical body absent from the selected snapshot\n",
    )
    .unwrap();
    host_git(&work, &["add", "."]);
    host_git(&work, &["commit", "-qm", "historical data"]);
    let retired = host_git(&work, &["rev-parse", "HEAD:retired"]);
    host_git(&work, &["rm", "-q", "retired"]);
    host_git(&work, &["commit", "-qm", "complete reusable ancestor"]);
    host_git(&work, &["push", "-q", remote.to_str().unwrap(), "main"]);
    let ancestor = host_git(&work, &["rev-parse", "HEAD"]);
    let cache = world.path().join("cache.git");
    host_git(
        world.path(),
        &[
            "clone",
            "-q",
            "--bare",
            "--no-hardlinks",
            remote.to_str().unwrap(),
            cache.to_str().unwrap(),
        ],
    );
    if shape == CacheShape::PrivateHead {
        let private = host_git(
            &cache,
            &[
                "commit-tree",
                &format!("{ancestor}^{{tree}}"),
                "-p",
                &ancestor,
                "-m",
                "unpublished cache work",
            ],
        );
        host_git(&cache, &["update-ref", "refs/heads/private", &private]);
        host_git(&cache, &["symbolic-ref", "HEAD", "refs/heads/private"]);
    }
    if snapshot_only {
        let historical = cache
            .join("objects")
            .join(&retired[..2])
            .join(&retired[2..]);
        assert!(historical.is_file());
        std::fs::remove_file(historical).unwrap();
        let empty_tree = host_git(&work, &["rev-parse", &format!("{ancestor}~2^{{tree}}")]);
        let historical_tree = cache
            .join("objects")
            .join(&empty_tree[..2])
            .join(&empty_tree[2..]);
        assert!(historical_tree.is_file());
        std::fs::remove_file(historical_tree).unwrap();
        assert!(
            host_git(
                &cache,
                &[
                    "rev-list",
                    "--objects",
                    "--quiet",
                    "--missing=print",
                    "--max-count=1",
                    &ancestor
                ]
            )
            .is_empty()
        );
        assert!(
            !raw_blob_tests::git_input(
                &cache,
                &["fsck", "--full", "--strict", "--no-dangling", &ancestor],
                None
            )
            .status
            .success()
        );
    }
    std::fs::write(work.join("new"), b"only the new tail belongs on the wire\n").unwrap();
    host_git(&work, &["add", "new"]);
    host_git(&work, &["commit", "-qm", "one missing selected body"]);
    host_git(&work, &["push", "-q", remote.to_str().unwrap(), "main"]);
    host_git(&remote, &["config", "uploadpack.allowFilter", "true"]);
    let wanted = host_git(&work, &["rev-parse", "HEAD"]);
    if shape == CacheShape::MissingSelected {
        let missing = host_git(&work, &["rev-parse", "HEAD:new"]);
        let path = remote
            .join("objects")
            .join(&missing[..2])
            .join(&missing[2..]);
        assert!(path.is_file());
        std::fs::remove_file(path).unwrap();
    }
    if dangling_wanted {
        let raw = raw_blob_tests::git_input(&work, &["cat-file", "commit", &wanted], None);
        assert!(raw.status.success());
        let stored = raw_blob_tests::git_input(
            &cache,
            &["hash-object", "-t", "commit", "-w", "--stdin"],
            Some(&raw.stdout),
        );
        assert!(stored.status.success());
        assert_eq!(String::from_utf8(stored.stdout).unwrap().trim(), wanted);
        if shape == CacheShape::WantedHead {
            host_git(&cache, &["update-ref", "refs/heads/main", &wanted]);
        }
        assert!(
            !raw_blob_tests::git_input(
                &cache,
                &["cat-file", "-e", &format!("{wanted}^{{tree}}")],
                None,
            )
            .status
            .success()
        );
    }
    let before = source_cache_tests::store_bytes(&cache);
    let expected_tip = if full && snapshot_only {
        // The historical snapshot is incomplete, while the original initial
        // empty commit is still a complete native Git history prefix.
        host_git(&work, &["rev-parse", &format!("{ancestor}~2")])
    } else {
        ancestor.clone()
    };
    {
        let name = if full { "full" } else { "checkout" };
        let receiver = world.path().join(format!("{name}.git"));
        host_git(
            world.path(),
            &["init", "-q", "--bare", receiver.to_str().unwrap()],
        );
        let receiver_before = source_cache_tests::store_bytes(&receiver);
        let bin = world.path().join(format!("{name}-bin"));
        std::fs::create_dir(&bin).unwrap();
        let packet = world.path().join(format!("{name}-packets"));
        let wire = world.path().join(format!("{name}-wire.pack"));
        // This wrapper changes only real Git's trace destinations. It executes
        // the host Git; the recorded PACK is the provider's actual transfer.
        let wrapper = bin.join("git");
        std::fs::write(&wrapper, format!(
            "#!/bin/sh\ncase \" $* \" in *' --refetch '*) ;;\n*' --no-filter '*|*' --depth=1 '*)\nexport GIT_TRACE_PACKET={}\nexport GIT_TRACE_PACKFILE={}\n;; esac\nexec {} \"$@\"\n",
            quoted(&packet), quoted(&wire), quoted(&host_git_path()),
        )).unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = std::env::join_paths(
            std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        let gh = world
            .client(git_env().with("PATH", path.to_str().unwrap()))
            .fx(Arc::new(Fx::simulated(live_start())))
            .git_remote_for_tests("fixture-dev/app", &remote)
            .build();
        let scope = if full {
            agh::git::FetchScope::FullSource {
                cache: cache.clone(),
            }
        } else {
            agh::git::FetchScope::SelectedCheckout {
                cache: Some(cache.clone()),
            }
        };
        let result = gh.run_git(&GitRun::fetch_ref(
            &common::slug("fixture-dev/app"),
            &receiver,
            &wanted,
            "refs/agh/delta",
            true,
            scope,
        ));
        if shape == CacheShape::MissingSelected {
            assert!(result.is_err(), "missing selected body must refuse");
            assert_eq!(source_cache_tests::store_bytes(&receiver), receiver_before);
            assert_eq!(source_cache_tests::store_bytes(&cache), before);
            assert!(gh.fx().children_run().iter().any(|call| {
                call.args
                    .iter()
                    .any(|arg| arg == format!("--negotiation-tip={ancestor}").as_str())
            }));
            assert!(gh.fx().children_run().iter().all(|call| {
                !call.args.iter().any(|arg| arg == "update-ref")
                    || call.cwd.as_deref().and_then(Path::file_name)
                        == Some(std::ffi::OsStr::new("cache-ancestor-export.git"))
            }));
            return;
        }
        result.unwrap_or_else(|error| panic!("{name}: {error:?}"));
        assert_eq!(
            host_git(&receiver, &["show", &format!("{wanted}:new")]),
            "only the new tail belongs on the wire"
        );
        assert_eq!(
            host_git(&receiver, &["rev-parse", "refs/agh/delta"]),
            wanted
        );
        assert!(!receiver.join("objects/info/alternates").exists());
        assert!(!receiver.join("shallow").exists());
        assert_eq!(
            host_git(&receiver, &["rev-list", "--parents", &wanted]),
            host_git(&work, &["rev-list", "--parents", &wanted]),
            "the receiver retains every original parent edge"
        );
        if full {
            host_git(
                &receiver,
                &["fsck", "--full", "--strict", "--no-dangling", &wanted],
            );
        }
        let bytes = std::fs::read(&wire).unwrap();
        assert!(
            bytes.starts_with(b"PACK"),
            "real {name} provider pack missing"
        );
        let count = u32::from_be_bytes(bytes[8..12].try_into().unwrap());
        eprintln!(
            "actual {name} provider pack: {count} objects, {} bytes; dangling wanted={dangling_wanted}",
            bytes.len()
        );
        let cold = full && snapshot_only;
        if cold {
            assert!(count > 32, "full source must acquire missing history");
        } else {
            assert!(
                count <= 6,
                "{name} transferred {count} objects, not the new tail"
            );
            assert!(
                bytes.len() < 16_384,
                "{name} retransferred cached stable bodies"
            );
        }
        let packets = std::fs::read_to_string(packet).unwrap();
        assert!(
            packets.contains(&format!("have {expected_tip}")),
            "{name}: no actual complete-prefix have packet"
        );
        if cold {
            assert!(
                !packets.contains(&format!("have {ancestor}")),
                "snapshot-only ancestor must not advertise complete history"
            );
        }
        let calls = gh.fx().children_run();
        let fetch = calls
            .iter()
            .find(|call| {
                call.args.iter().any(|arg| {
                    arg == format!(
                        "+{wanted}:refs/agh/{}",
                        if full { "source" } else { "checkout-body" }
                    )
                    .as_str()
                }) && call
                    .args
                    .iter()
                    .any(|arg| arg == "--no-filter" || arg == "--depth=1")
            })
            .unwrap();
        assert!(
            fetch
                .args
                .iter()
                .any(|arg| arg == format!("--negotiation-tip={expected_tip}").as_str())
        );
        assert!(
            !fetch
                .args
                .iter()
                .any(|arg| arg == "fetch.negotiationAlgorithm=noop")
        );
        assert_eq!(source_cache_tests::store_bytes(&cache), before);
    }
}

#[test]
fn near_current_cache_transfers_only_the_new_selected_body() {
    check(CacheShape::Complete, false);
}

#[test]
fn near_current_cache_completes_full_source_with_only_the_new_tail() {
    check(CacheShape::Complete, true);
}

#[test]
fn dangling_wanted_commit_never_becomes_a_complete_cache_have() {
    check(CacheShape::DanglingWanted, false);
}

#[test]
fn full_source_excludes_dangling_wanted_commit_from_its_haves() {
    check(CacheShape::DanglingWanted, true);
}

#[test]
fn checkout_reuses_a_complete_snapshot_without_its_historical_bodies() {
    check(CacheShape::Snapshot, false);
}

#[test]
fn full_source_only_advertises_an_actual_complete_history_prefix() {
    check(CacheShape::Snapshot, true);
}

#[test]
fn cached_snapshot_cannot_repair_an_absent_new_selected_body() {
    check(CacheShape::MissingSelected, false);
}

#[test]
fn private_head_keeps_a_genuine_selected_ancestor_available() {
    check(CacheShape::PrivateHead, false);
}

#[test]
fn private_head_keeps_a_genuine_full_source_ancestor_available() {
    check(CacheShape::PrivateHead, true);
}

#[test]
fn wanted_head_uses_original_selected_parent_data_without_a_retained_ref() {
    check(CacheShape::WantedHead, false);
}

#[test]
fn wanted_head_uses_original_full_history_parent_data_without_a_retained_ref() {
    check(CacheShape::WantedHead, true);
}
