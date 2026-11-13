// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use super::*;
use agh::git::{AncestryQuery, CommitRead, CommitReadInput};

fn object(dir: &Path, oid: &str) -> std::process::Output {
    let mut command = aterm_uds::spawnfd::command(host_git_path());
    command
        .args(["cat-file", "-e", oid])
        .current_dir(dir)
        .env_clear()
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    aterm_uds::deadline::output_until(&mut command, std::time::Instant::now() + common::HANG)
        .expect("bounded local Git read")
}

/// Git admits the sixth alternate from the source, but adding the scratch's
/// source edge hides it. The seventh store must remain outside the read set.
fn deep_source(world: &World, source: &Path, accessible: &str) -> String {
    let common = PathBuf::from(host_git(
        source,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    ));
    let primary = common.join("objects");
    let beyond = bare_repo(world.path(), "beyond-source-depth");
    let content = world.path().join("excluded-content");
    std::fs::write(
        &content,
        "only the inaccessible seventh alternate holds this blob\n",
    )
    .unwrap();
    let excluded = host_git(&beyond, &["hash-object", "-w", content.to_str().unwrap()]);
    let mut stores = vec![primary.clone()];
    for index in 1..=6 {
        // Exercise Git's C-quoted inventory path and alternate-file syntax.
        stores.push(world.path().join(format!("alternate\t{index}")));
    }
    std::fs::rename(&primary, &stores[6]).unwrap();
    stores.push(beyond.join("objects"));
    for store in stores.iter().take(6) {
        std::fs::create_dir_all(store.join("info")).unwrap();
        std::fs::create_dir_all(store.join("pack")).unwrap();
    }
    for index in 0..7 {
        std::fs::write(
            stores[index].join("info/alternates"),
            format!(
                "{}\n",
                aterm_json::to_string(stores[index + 1].to_str().unwrap()).unwrap()
            ),
        )
        .unwrap();
    }
    assert!(
        object(source, accessible).status.success(),
        "the genuine source holds the selected object"
    );
    assert!(
        !object(source, &excluded).status.success(),
        "the source cannot read beyond its depth boundary"
    );
    let old = world.path().join("old-added-edge.git");
    host_git(
        world.path(),
        &["init", "--quiet", "--bare", old.to_str().unwrap()],
    );
    std::fs::write(
        old.join("objects/info/alternates"),
        format!("{}\n", primary.display()),
    )
    .unwrap();
    let missing = object(&old, accessible);
    assert!(
        !missing.status.success(),
        "the former destination setup must reproduce the failure"
    );
    assert!(String::from_utf8_lossy(&missing.stderr).contains("nesting too deep"));
    excluded
}

#[test]
fn native_push_preserves_the_source_object_depth_boundary() {
    use std::sync::{Mutex, OnceLock, Weak};
    let world = World::canonical("push-source-object-depth");
    common::canonical_repos(&world.fake);
    let (remote, source, before, after) = incremental_source(&world);
    let excluded = deep_source(&world, &source, &after);
    let refs = host_git(&source, &["for-each-ref"]);
    let seen = Arc::new(Mutex::new(false));
    let record = Arc::clone(&seen);
    let effects = Arc::new(OnceLock::<Weak<Fx>>::new());
    let observed = Arc::clone(&effects);
    let wanted = after.clone();
    let fx = Arc::new(
        Fx::simulated(live_start()).with_git_child_observer_for_tests(move |span| {
            let fx = observed.get().and_then(Weak::upgrade).unwrap();
            let spec = &fx.children_run()[span.ordinal];
            if spec.args.iter().any(|arg| arg == "pack-objects") {
                let scratch = spec.cwd.as_ref().expect("private pack view");
                assert!(object(scratch, &wanted).status.success());
                assert!(
                    !object(scratch, &excluded).status.success(),
                    "flattening must not admit the seventh source store"
                );
                *record.lock().unwrap() = true;
            }
        }),
    );
    assert!(effects.set(Arc::downgrade(&fx)).is_ok());
    let gh = world
        .client(git_env())
        .fx(fx)
        .git_remote_for_tests("fixture-dev/app", &remote)
        .build();
    gh.run_git(&GitRun::push_refs(
        &common::slug("fixture-dev/app"),
        &source,
        push_batch(&[("refs/heads/main", &before, &after)]),
        false,
    ))
    .unwrap_or_else(|error| panic!("{}", error.why));
    assert!(
        *seen.lock().unwrap(),
        "the real source-independent pack path ran"
    );
    assert_eq!(host_git(&remote, &["rev-parse", "main"]), after);
    assert_eq!(host_git(&source, &["for-each-ref"]), refs);
    assert_eq!(
        host_git(&remote, &["show", "main:new"]),
        "new object that must be checked"
    );
}

#[test]
fn native_fetch_and_checkout_cache_keep_depth_boundary_objects_available() {
    let world = World::canonical("fetch-source-object-depth");
    common::canonical_repos(&world.fake);
    let (remote, source, before, after) = incremental_source(&world);
    let excluded = deep_source(&world, &source, &after);
    let gh = world
        .client(git_env())
        .git_remote_for_tests("fixture-dev/app", &remote)
        .build();
    let slug = common::slug("fixture-dev/app");
    gh.run_git(&GitRun::fetch_ref(
        &slug,
        &source,
        &before,
        "refs/agh/deep",
        true,
        agh::git::FetchScope::NativeExact,
    ))
    .unwrap_or_else(|error| panic!("{}", error.why));
    assert_eq!(host_git(&source, &["rev-parse", "refs/agh/deep"]), before);
    assert_eq!(host_git(&source, &["rev-parse", "main"]), after);
    let receiver = world.path().join("checkout.git");
    host_git(
        world.path(),
        &["init", "--quiet", "--bare", receiver.to_str().unwrap()],
    );
    gh.run_git(&GitRun::fetch_ref(
        &slug,
        &receiver,
        &before,
        "refs/agh/checkout",
        true,
        agh::git::FetchScope::SelectedCheckout {
            cache: Some(source.clone()),
        },
    ))
    .unwrap_or_else(|error| panic!("{}", error.why));
    assert_eq!(
        host_git(&receiver, &["show", "refs/agh/checkout:old"]),
        "the authenticated old blob"
    );
    assert!(!object(&source, &excluded).status.success());
    assert!(!object(&receiver, &excluded).status.success());
}

#[test]
fn native_commit_ancestry_reuses_the_source_depth_boundary_without_acquisition() {
    let world = World::canonical("ancestry-source-object-depth");
    let (_, source, before, after) = incremental_source(&world);
    deep_source(&world, &source, &after);
    let request = CommitRead::new(CommitReadInput {
        commits: Vec::new(),
        queries: vec![AncestryQuery {
            ancestor: before,
            descendant: after,
        }],
    })
    .unwrap();
    let gh = world.client(git_env()).build();
    let out = gh
        .run_git(&GitRun::commit_read(
            &common::slug("fixture-dev/app"),
            &source,
            request,
            true,
        ))
        .unwrap_or_else(|error| panic!("{}", error.why));
    let value: aterm_json::Value = aterm_json::from_str(&out.stdout).unwrap();
    assert_eq!(value["acquired_commits"], aterm_json::json!(0));
    assert_eq!(value["queries"][0]["contained"], aterm_json::json!(true));
}
