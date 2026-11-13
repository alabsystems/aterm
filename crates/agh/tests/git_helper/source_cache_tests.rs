// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use super::*;

#[test]
fn source_object_cache_fetch_retains_all_historical_blobs_without_sharing_cache_files() {
    let world = World::canonical("helper-full-source-cache");
    let bare = crate::git_runner::bare_repo(world.root.path(), "history");
    let work = world.root.path().join("history-work");
    std::fs::write(work.join("historical"), b"introduced then deleted\n").unwrap();
    crate::git_runner::host_git(&work, &["add", "."]);
    crate::git_runner::host_git(&work, &["commit", "-qm", "historical bytes"]);
    let middle = crate::git_runner::host_git(&work, &["rev-parse", "HEAD"]);
    let historical = crate::git_runner::host_git(&work, &["rev-parse", "HEAD:historical"]);
    crate::git_runner::host_git(&work, &["push", "-q", bare.to_str().unwrap(), "HEAD:main"]);
    let cache = world.root.path().join("cache.git");
    crate::git_runner::host_git(
        world.root.path(),
        &[
            "clone",
            "-q",
            "--bare",
            "--no-hardlinks",
            bare.to_str().unwrap(),
            cache.to_str().unwrap(),
        ],
    );
    crate::git_runner::host_git(&work, &["rm", "-q", "historical"]);
    std::fs::write(work.join("current"), b"new remote-only bytes\n").unwrap();
    crate::git_runner::host_git(&work, &["add", "."]);
    crate::git_runner::host_git(&work, &["commit", "-qm", "selected snapshot"]);
    crate::git_runner::host_git(&work, &["push", "-q", bare.to_str().unwrap(), "HEAD:main"]);
    crate::git_runner::host_git(&bare, &["config", "uploadpack.allowFilter", "true"]);
    crate::git_runner::host_git(&bare, &["config", "uploadpack.allowAnySHA1InWant", "true"]);
    let target = crate::git_runner::host_git(&work, &["rev-parse", "HEAD"]);
    let shallow_receiver = world.root.path().join("shallow-receiver.git");
    let remote_url = format!("file://{}", bare.display());
    crate::git_runner::host_git(
        world.root.path(),
        &[
            "clone",
            "-q",
            "--bare",
            "--depth=1",
            "--branch=main",
            &remote_url,
            shallow_receiver.to_str().unwrap(),
        ],
    );
    assert_eq!(
        crate::git_runner::host_git(&shallow_receiver, &["rev-parse", "--is-shallow-repository"]),
        "true"
    );
    let shallow_bytes = std::fs::read(shallow_receiver.join("shallow")).unwrap();
    let shallow_config = std::fs::read(shallow_receiver.join("config")).unwrap();
    let shallow_refs = crate::git_runner::host_git(&shallow_receiver, &["show-ref"]);
    // Exercise CLI refusal when the selected incomplete-cache plan cannot
    // acquire the full history from the authoritative source.
    let remote_historical = bare
        .join("objects")
        .join(&historical[..2])
        .join(&historical[2..]);
    assert!(
        remote_historical.is_file(),
        "tiny native push exposes its historical loose object"
    );
    let remote_historical_bytes = std::fs::read(&remote_historical).unwrap();
    std::fs::remove_file(&remote_historical).unwrap();
    let receiver = world.root.path().join("receiver.git");
    crate::git_runner::host_git(
        world.root.path(),
        &["init", "-q", "--bare", receiver.to_str().unwrap()],
    );
    let config = std::fs::read(receiver.join("config")).unwrap();
    let cache_config = std::fs::read(cache.join("config")).unwrap();
    let cache_refs = crate::git_runner::host_git(&cache, &["show-ref"]);
    let invoke = |receiver: &Path| {
        let (ctx, captured) = world.ctx(crate::git_runner::git_env());
        let mut ctx = ctx
            .with_git_helper(common::helper())
            .with_git_remote("alabsystems/aterm", &bare);
        let exit = common::run_cli(
            &mut ctx,
            &[
                "repo",
                "fetch",
                "alabsystems/aterm",
                receiver.to_str().unwrap(),
                "--ref",
                &target,
                "--into",
                "refs/agh/full-source",
                "--source-object-cache",
                cache.to_str().unwrap(),
                "--anonymous",
            ],
        );
        (exit, captured)
    };
    let (exit, _) = invoke(&shallow_receiver);
    assert_ne!(
        exit, 0,
        "a shallow caller cannot receive a healthy full-source publication"
    );
    assert_eq!(
        std::fs::read(shallow_receiver.join("shallow")).unwrap(),
        shallow_bytes
    );
    assert_eq!(
        std::fs::read(shallow_receiver.join("config")).unwrap(),
        shallow_config
    );
    assert_eq!(
        crate::git_runner::host_git(&shallow_receiver, &["show-ref"]),
        shallow_refs
    );
    let promisor_receiver = world.root.path().join("promisor-receiver.git");
    crate::git_runner::host_git(
        world.root.path(),
        &["init", "-q", "--bare", promisor_receiver.to_str().unwrap()],
    );
    let marker = promisor_receiver.join("objects/pack/pack-untrusted.promisor");
    std::fs::write(&marker, b"caller-owned promise\n").unwrap();
    let (exit, _) = invoke(&promisor_receiver);
    assert_ne!(
        exit, 0,
        "caller promises cannot exempt a full-source receiver from integrity"
    );
    assert_eq!(std::fs::read(marker).unwrap(), b"caller-owned promise\n");
    assert!(crate::git_runner::host_git(&promisor_receiver, &["for-each-ref"]).is_empty());
    let replaced_receiver = world.root.path().join("packed-replacement-receiver.git");
    crate::git_runner::host_git(
        world.root.path(),
        &[
            "clone",
            "-q",
            "--bare",
            "--no-hardlinks",
            cache.to_str().unwrap(),
            replaced_receiver.to_str().unwrap(),
        ],
    );
    let replacement = format!("refs/replace/{middle}");
    let replacement_parent =
        crate::git_runner::host_git(&cache, &["rev-parse", &format!("{middle}^")]);
    crate::git_runner::host_git(
        &replaced_receiver,
        &["update-ref", &replacement, &replacement_parent],
    );
    crate::git_runner::host_git(&replaced_receiver, &["pack-refs", "--all", "--prune"]);
    let loose_replacements = replaced_receiver.join("refs/replace");
    if loose_replacements.exists() {
        std::fs::remove_dir(loose_replacements).unwrap();
    }
    assert!(!replaced_receiver.join("refs/replace").exists());
    let packed_refs = std::fs::read(replaced_receiver.join("packed-refs")).unwrap();
    let (exit, _) = invoke(&replaced_receiver);
    assert_ne!(
        exit, 0,
        "packed replacements cannot change full-source receiver identity"
    );
    assert_eq!(
        std::fs::read(replaced_receiver.join("packed-refs")).unwrap(),
        packed_refs
    );
    let incomplete_receiver = world.root.path().join("incomplete-source-receiver.git");
    crate::git_runner::host_git(
        world.root.path(),
        &[
            "init",
            "-q",
            "--bare",
            incomplete_receiver.to_str().unwrap(),
        ],
    );
    let cache_before = crate::git_runner::source_cache_tests::store_bytes(&cache);
    let receiver_before = crate::git_runner::source_cache_tests::store_bytes(&incomplete_receiver);
    let (exit, captured) = invoke(&incomplete_receiver);
    assert_ne!(exit, 0, "{}", captured.stderr_text());
    assert_eq!(
        crate::git_runner::source_cache_tests::store_bytes(&cache),
        cache_before
    );
    assert_eq!(
        crate::git_runner::source_cache_tests::store_bytes(&incomplete_receiver),
        receiver_before
    );
    // Change only the owned provider input before a distinct positive receiver.
    std::fs::write(&remote_historical, &remote_historical_bytes).unwrap();
    crate::git_runner::host_git(
        &bare,
        &["fsck", "--full", "--strict", "--no-dangling", &target],
    );
    let (exit, captured) = invoke(&receiver);
    assert_eq!(exit, 0, "{}", captured.stderr_text());
    assert_eq!(
        crate::git_runner::host_git(&receiver, &["show", &format!("{middle}:historical")]),
        "introduced then deleted"
    );
    assert_eq!(
        crate::git_runner::host_git(&receiver, &["show", &format!("{target}:current")]),
        "new remote-only bytes"
    );
    crate::git_runner::host_git(
        &receiver,
        &["fsck", "--full", "--strict", "--no-dangling", &target],
    );
    assert_eq!(
        crate::git_runner::host_git(&receiver, &["rev-parse", "--is-shallow-repository"]),
        "false"
    );
    assert!(!receiver.join("objects/info/alternates").exists());
    assert!(
        !std::fs::read_dir(receiver.join("objects/pack"))
            .unwrap()
            .any(|entry| entry
                .unwrap()
                .path()
                .extension()
                .is_some_and(|ext| ext == "promisor"))
    );
    assert_eq!(std::fs::read(receiver.join("config")).unwrap(), config);
    assert_eq!(std::fs::read(cache.join("config")).unwrap(), cache_config);
    assert_eq!(
        crate::git_runner::host_git(&cache, &["show-ref"]),
        cache_refs
    );
    use std::os::unix::fs::MetadataExt as _;
    let original = cache
        .join("objects")
        .join(&historical[..2])
        .join(&historical[2..]);
    let original_meta = std::fs::metadata(&original).unwrap();
    for entry in std::fs::read_dir(receiver.join("objects/pack")).unwrap() {
        let metadata = entry.unwrap().metadata().unwrap();
        assert_ne!(
            (metadata.dev(), metadata.ino()),
            (original_meta.dev(), original_meta.ino())
        );
    }
    // Idempotent independent DATA import must accept existing equal pack bytes.
    let (exit, captured) = invoke(&receiver);
    assert_eq!(exit, 0, "{}", captured.stderr_text());
    let unsafe_receiver = world.root.path().join("unsafe-receiver.git");
    crate::git_runner::host_git(
        world.root.path(),
        &["init", "-q", "--bare", unsafe_receiver.to_str().unwrap()],
    );
    let victim = world.root.path().join("unowned-pack-parent");
    std::fs::create_dir(&victim).unwrap();
    std::fs::write(victim.join("sentinel"), b"preserve").unwrap();
    std::fs::remove_dir(unsafe_receiver.join("objects/pack")).unwrap();
    std::os::unix::fs::symlink(&victim, unsafe_receiver.join("objects/pack")).unwrap();
    let (exit, _) = invoke(&unsafe_receiver);
    assert_ne!(exit, 0);
    assert_eq!(std::fs::read(victim.join("sentinel")).unwrap(), b"preserve");
    assert_eq!(std::fs::read_dir(&victim).unwrap().count(), 1);
    let corrupt_receiver = world.root.path().join("corrupt-cache-receiver.git");
    crate::git_runner::host_git(
        world.root.path(),
        &["init", "-q", "--bare", corrupt_receiver.to_str().unwrap()],
    );
    let mut permissions = std::fs::metadata(&original).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt as _;
    permissions.set_mode(0o600);
    std::fs::set_permissions(&original, permissions).unwrap();
    std::fs::write(&original, b"corrupt cache object").unwrap();
    let (exit, _) = invoke(&corrupt_receiver);
    assert_ne!(
        exit, 0,
        "corrupt cache DATA must not silently become an ordinary miss"
    );
    assert!(crate::git_runner::host_git(&corrupt_receiver, &["for-each-ref"]).is_empty());
    assert_eq!(std::fs::read(&original).unwrap(), b"corrupt cache object");
}

#[test]
fn cached_commit_never_substitutes_for_the_selected_remote_repository() {
    let world = World::canonical("helper-source-cache-authority");
    let remote = crate::git_runner::bare_repo(world.root.path(), "selected-remote");
    let cache = crate::git_runner::bare_repo(world.root.path(), "other-cache");
    let work = world.root.path().join("other-cache-work");
    std::fs::write(
        work.join("unpublished"),
        b"belongs only to the other source\n",
    )
    .unwrap();
    crate::git_runner::host_git(&work, &["add", "."]);
    crate::git_runner::host_git(&work, &["commit", "-qm", "other repository"]);
    crate::git_runner::host_git(&work, &["push", "-q", cache.to_str().unwrap(), "HEAD:main"]);
    let target = crate::git_runner::host_git(&cache, &["rev-parse", "main"]);
    let receiver = world.root.path().join("receiver.git");
    crate::git_runner::host_git(
        world.root.path(),
        &["init", "-q", "--bare", receiver.to_str().unwrap()],
    );
    let (ctx, captured) = world.ctx(crate::git_runner::git_env());
    let mut ctx = ctx
        .with_git_helper(common::helper())
        .with_git_remote("alabsystems/aterm", &remote);
    let exit = common::run_cli(
        &mut ctx,
        &[
            "repo",
            "fetch",
            "alabsystems/aterm",
            receiver.to_str().unwrap(),
            "--ref",
            &target,
            "--into",
            "refs/agh/full-source",
            "--source-object-cache",
            cache.to_str().unwrap(),
            "--anonymous",
        ],
    );
    assert_ne!(
        exit,
        0,
        "cache refs cannot attest remote ownership: {}",
        captured.stderr_text()
    );
    assert!(crate::git_runner::host_git(&receiver, &["for-each-ref"]).is_empty());
}

#[test]
fn source_and_checkout_scopes_require_exact_refs_and_cannot_mix() {
    let world = World::canonical("helper-source-cache-contract");
    for (extra, why) in [
        (
            vec![
                "--source-object-cache",
                "/not-opened",
                "--ref",
                "refs/heads/main",
            ],
            "exact immutable",
        ),
        (
            vec!["--checkout-closure", "--ref", "refs/heads/main"],
            "exact immutable",
        ),
        (
            vec![
                "--checkout-cache",
                "/not-opened",
                "--ref",
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            ],
            "requires --checkout-closure",
        ),
        (
            vec![
                "--source-object-cache",
                "/not-opened",
                "--checkout-closure",
                "--ref",
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            ],
            "mutually exclusive",
        ),
    ] {
        let (mut ctx, captured) = world.ctx(crate::git_runner::git_env());
        let mut args = vec![
            "repo",
            "fetch",
            "alabsystems/aterm",
            "/not-opened",
            "--into",
            "refs/agh/exact",
        ];
        args.extend(extra);
        assert_eq!(common::run_cli(&mut ctx, &args), 2);
        assert!(
            captured.stderr_text().contains(why),
            "{}",
            captured.stderr_text()
        );
    }
    assert!(world.fake.seen().is_empty());
    assert!(!world.dirs.grants().exists());
}
