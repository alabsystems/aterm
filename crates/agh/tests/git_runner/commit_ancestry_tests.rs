// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use super::*;
use agh::git::{AncestryQuery, CommitRead, CommitReadInput};
use agh_fake::{Fault, Match};
use aterm_json::{Value, json};

fn request(ancestor: &str, descendant: &str) -> CommitRead {
    CommitRead::new(CommitReadInput {
        commits: Vec::new(),
        queries: vec![AncestryQuery {
            ancestor: ancestor.into(),
            descendant: descendant.into(),
        }],
    })
    .unwrap()
}

fn api_commit(world: &World, donor: &Path, id: &str) {
    let raw = host_git(donor, &["cat-file", "commit", id]);
    let (header, message) = raw.split_once("\n\n").unwrap();
    let parents = header
        .lines()
        .filter_map(|s| s.strip_prefix("parent "))
        .map(|sha| json!({"sha":sha}))
        .collect::<Vec<_>>();
    let value = json!({"sha":id,"tree":{"sha":host_git(donor,&["rev-parse",&format!("{id}^{{tree}}")])},"parents":parents,
        "author":{"name":"fixture","email":"fixture","date":"2023-11-14T22:13:20Z"},
        "committer":{"name":"fixture","email":"fixture","date":"2023-11-14T22:13:20Z"},
        "message":message,"verification":{"payload":null,"signature":null}});
    world.fake.fault_always(
        Match::api(&format!("/repos/fixture-dev/app/git/commits/{id}")),
        Fault::Status {
            status: 200,
            body: aterm_json::to_string(&value).unwrap(),
            headers: Vec::new(),
        },
    );
}

fn answer(
    gh: &Github,
    store: &Path,
    ancestor: &str,
    descendant: &str,
) -> Result<Value, agh::Error> {
    let out = gh.run_git(&GitRun::commit_read(
        &common::slug("fixture-dev/app"),
        store,
        request(ancestor, descendant),
        true,
    ))?;
    Ok(aterm_json::from_str(&out.stdout).unwrap())
}

/// The explicit-public commit reader owns the same bounded retry policy as every other
/// anonymous API read. NEGATIVE CONTROL: restoring `RetryPolicy::NONE` in the ancestry reader
/// refuses on the first injected 504, so neither the three calls nor the two retry records exist.
#[test]
fn explicit_public_commit_acquisition_retries_two_504s_then_succeeds() {
    let world = World::canonical("commit-ancestry-retry");
    let donor = bare_repo(world.path(), "donor");
    let first = host_git(&donor, &["rev-parse", "main"]);
    let tree = host_git(&donor, &["rev-parse", "main^{tree}"]);
    let store = world.path().join("store.git");
    host_git(
        world.path(),
        &[
            "clone",
            "--quiet",
            "--bare",
            &donor.to_string_lossy(),
            &store.to_string_lossy(),
        ],
    );
    let second = host_git(
        &donor,
        &["commit-tree", &tree, "-p", &first, "-m", "second"],
    );
    api_commit(&world, &donor, &second);
    let path = format!("/repos/fixture-dev/app/git/commits/{second}");
    for _ in 0..2 {
        world.fake.fault(
            Match::api(&path),
            Fault::Status {
                status: 504,
                body: "temporary gateway failure".into(),
                headers: Vec::new(),
            },
        );
    }

    let gh = world.client(git_env()).build();
    let out = gh
        .run_git(
            &GitRun::commit_read(
                &common::slug("fixture-dev/app"),
                &store,
                request(&first, &second),
                true,
            )
            .with_budget(Duration::from_secs(1)),
        )
        .expect("the third acquisition succeeds after twenty seconds of recorded retry waits");
    let result: Value = aterm_json::from_str(&out.stdout).unwrap();
    assert_eq!(result["queries"][0]["contained"], json!(true));
    assert_eq!(result["acquired_commits"], json!(1));
    assert_eq!(
        world
            .fake
            .seen()
            .iter()
            .filter(|seen| seen.path_and_query == path)
            .count(),
        3
    );
    assert_eq!(gh.fx().retry_notes().len(), 2);
    assert!(
        gh.fx()
            .retry_notes()
            .iter()
            .all(|note| note.contains("HTTP 504"))
    );
    let children = gh.fx().children_run();
    assert!(!children.is_empty());
    assert!(children.iter().all(|child| {
        child
            .progress
            .as_ref()
            .is_some_and(|progress| progress.window == Duration::from_secs(60))
    }));
}

#[test]
fn moving_tip_acquires_only_exact_commits_not_large_trees_or_blob_history() {
    let world = World::canonical("commit-ancestry-moving");
    let donor = bare_repo(world.path(), "donor");
    let first = host_git(&donor, &["rev-parse", "main"]);
    let store = world.path().join("store.git");
    host_git(
        world.path(),
        &[
            "clone",
            "--quiet",
            "--bare",
            &donor.to_string_lossy(),
            &store.to_string_lossy(),
        ],
    );
    let before = host_git(&store, &["show-ref"]);
    let config = std::fs::read(store.join("config")).unwrap();
    std::fs::write(store.join("FETCH_HEAD"), b"original fetch head\n").unwrap();
    let blob_input = world.path().join("large-blob");
    std::fs::write(&blob_input, vec![0xa5; 4 * 1024 * 1024]).unwrap();
    let blob = host_git(
        &donor,
        &["hash-object", "-w", &blob_input.to_string_lossy()],
    );
    let mut tree = b"100644 large\0".to_vec();
    // Native Git constructs and validates the real tree used by the tip.
    for part in blob.as_bytes().as_chunks::<2>().0 {
        tree.push(u8::from_str_radix(std::str::from_utf8(part).unwrap(), 16).unwrap());
    }
    let tree_input = world.path().join("tree-object");
    std::fs::write(&tree_input, tree).unwrap();
    let tree = host_git(
        &donor,
        &[
            "hash-object",
            "-w",
            "-t",
            "tree",
            &tree_input.to_string_lossy(),
        ],
    );
    let second = host_git(
        &donor,
        &["commit-tree", &tree, "-p", &first, "-m", "second"],
    );
    api_commit(&world, &donor, &second);
    let gh = world.client(git_env()).build();
    let result = answer(&gh, &store, &first, &second).unwrap();
    assert_eq!(result["queries"][0]["contained"], json!(true));
    assert_eq!(result["acquired_commits"], json!(1));
    assert!(result["api_body_bytes"].as_u64().unwrap() < 4096);
    assert_eq!(result["checkout_complete"], json!(false));
    assert_eq!(host_git(&store, &["show-ref"]), before);
    assert_eq!(std::fs::read(store.join("config")).unwrap(), config);
    assert_eq!(
        std::fs::read(store.join("FETCH_HEAD")).unwrap(),
        b"original fetch head\n"
    );
    // No source fetch occurs, and native cat-file proves the large tip inputs
    // remain absent rather than merely inferring that from transfer size.
    let mut missing = aterm_uds::spawnfd::command(host_git_path());
    missing.args([
        "--git-dir",
        &store.to_string_lossy(),
        "cat-file",
        "-e",
        &blob,
    ]);
    assert!(
        !aterm_uds::deadline::output_until(&mut missing, std::time::Instant::now() + common::HANG)
            .unwrap()
            .status
            .success()
    );
    let third = host_git(
        &donor,
        &["commit-tree", &tree, "-p", &second, "-m", "third"],
    );
    api_commit(&world, &donor, &third);
    host_git(&donor, &["update-ref", "refs/heads/main", &third]);
    let again = answer(&gh, &store, &first, &second).unwrap();
    assert_eq!(again["queries"][0]["descendant"], json!(second));
    assert_eq!(again["acquired_commits"], json!(0));
    let moved = answer(&gh, &store, &first, &third).unwrap();
    assert_eq!(moved["queries"][0]["contained"], json!(true));
    assert_eq!(moved["acquired_commits"], json!(1));
    assert_eq!(host_git(&store, &["show-ref"]), before);
    assert!(gh.fx().children_run().iter().all(|s| {
        !s.args
            .iter()
            .any(|a| a == "fetch" || a == "clone" || a == "index-pack" || a == "update-ref")
    }));
}

#[test]
fn native_negative_requires_original_complete_parent_graph_and_ignores_source_shallow_and_grafts() {
    let world = World::canonical("commit-ancestry-negative");
    let store = bare_repo(world.path(), "source");
    let first = host_git(&store, &["rev-parse", "main"]);
    let tree = host_git(&store, &["rev-parse", "main^{tree}"]);
    let tip = host_git(&store, &["commit-tree", &tree, "-p", &first, "-m", "tip"]);
    let fork = host_git(&store, &["commit-tree", &tree, "-m", "unrelated"]);
    std::fs::write(store.join("shallow"), format!("{tip}\n")).unwrap();
    std::fs::write(store.join("info/grafts"), format!("{tip} {fork}\n")).unwrap();
    let gh = world.client(git_env()).build();
    assert_eq!(
        answer(&gh, &store, &first, &tip).unwrap()["queries"][0]["contained"],
        json!(true)
    );
    let result = answer(&gh, &store, &fork, &tip).unwrap();
    assert_eq!(result["queries"][0]["contained"], json!(false));
    assert_eq!(result["api_body_bytes"], json!(0));
    assert_eq!(
        std::fs::read(store.join("shallow")).unwrap(),
        format!("{tip}\n").as_bytes()
    );
    assert_eq!(
        std::fs::read(store.join("info/grafts")).unwrap(),
        format!("{tip} {fork}\n").as_bytes()
    );
}

#[test]
fn missing_parent_or_unknown_api_bytes_never_become_negative_or_worker_ref_changes() {
    let world = World::canonical("commit-ancestry-gap");
    let store = bare_repo(world.path(), "source");
    let first = host_git(&store, &["rev-parse", "main"]);
    let tree = host_git(&store, &["rev-parse", "main^{tree}"]);
    let missing = "a".repeat(40);
    let raw = format!(
        "tree {tree}\nparent {missing}\nauthor fixture <fixture> 1700000000 +0000\ncommitter fixture <fixture> 1700000000 +0000\n\ngap\n"
    );
    let input = world.path().join("gap-commit");
    std::fs::write(&input, raw).unwrap();
    let tip = host_git(
        &store,
        &[
            "hash-object",
            "-w",
            "-t",
            "commit",
            &input.to_string_lossy(),
        ],
    );
    let refs = host_git(&store, &["show-ref"]);
    world.fake.fault_always(
        Match::api(&format!("/repos/fixture-dev/app/git/commits/{missing}")),
        Fault::Status {
            status: 200,
            body: format!("{{\"sha\":\"{first}\"}}"),
            headers: Vec::new(),
        },
    );
    let gh = world.client(git_env()).build();
    assert!(answer(&gh, &store, &first, &tip).is_err());
    assert_eq!(host_git(&store, &["show-ref"]), refs);
    assert!(
        gh.fx()
            .children_run()
            .iter()
            .all(|s| !s.args.iter().any(|a| a == "fetch" || a == "update-ref"))
    );
}

#[test]
fn missing_parent_positive_is_acquired_without_changing_the_original_shallow_boundary() {
    let world = World::canonical("commit-ancestry-cold");
    let donor = bare_repo(world.path(), "donor");
    let first = host_git(&donor, &["rev-parse", "main"]);
    let tree = host_git(&donor, &["rev-parse", "main^{tree}"]);
    let store = world.path().join("cold.git");
    host_git(
        world.path(),
        &[
            "clone",
            "--quiet",
            "--bare",
            &donor.to_string_lossy(),
            &store.to_string_lossy(),
        ],
    );
    let middle = host_git(
        &donor,
        &["commit-tree", &tree, "-p", &first, "-m", "middle"],
    );
    let tip = host_git(&donor, &["commit-tree", &tree, "-p", &middle, "-m", "tip"]);
    api_commit(&world, &donor, &middle);
    api_commit(&world, &donor, &tip);
    std::fs::write(store.join("shallow"), format!("{tip}\n")).unwrap();
    let gh = world.client(git_env()).build();
    let result = answer(&gh, &store, &first, &tip).unwrap();
    assert_eq!(result["queries"][0]["contained"], json!(true));
    assert_eq!(result["acquired_commits"], json!(2));
    assert!(result["api_body_bytes"].as_u64().unwrap() < 8192);
    assert_eq!(
        std::fs::read(store.join("shallow")).unwrap(),
        format!("{tip}\n").as_bytes()
    );
    assert_eq!(host_git(&store, &["rev-parse", "main"]), first);
}

#[test]
fn a_caller_cannot_replace_commit_ancestry_s_owned_progress_bound() {
    let world = World::canonical("commit-ancestry-progress-owned");
    let store = bare_repo(world.path(), "source");
    let first = host_git(&store, &["rev-parse", "main"]);
    let gh = world.client(git_env()).build();
    let error = gh
        .run_git(
            &GitRun::commit_read(
                &common::slug("fixture-dev/app"),
                &store,
                request(&first, &first),
                true,
            )
            .with_progress_window(Duration::from_secs(3600)),
        )
        .unwrap_err();
    assert_eq!(error.kind, Kind::Usage);
    assert!(gh.fx().children_run().is_empty());
}

#[test]
fn complete_descendant_answers_do_not_require_ancestor_side_history() {
    let world = World::canonical("commit-ancestry-ancestor-gap");
    let store = bare_repo(world.path(), "source");
    let root = host_git(&store, &["rev-parse", "main"]);
    let tree = host_git(&store, &["rev-parse", "main^{tree}"]);
    let missing = "a".repeat(40);
    let raw = format!(
        "tree {tree}\nparent {missing}\nauthor fixture <fixture> 1700000000 +0000\ncommitter fixture <fixture> 1700000000 +0000\n\nancestor with unavailable history\n"
    );
    let input = world.path().join("ancestor-commit");
    std::fs::write(&input, raw).unwrap();
    let ancestor = host_git(
        &store,
        &[
            "hash-object",
            "-w",
            "-t",
            "commit",
            &input.to_string_lossy(),
        ],
    );
    let refs = host_git(&store, &["show-ref"]);
    let config = std::fs::read(store.join("config")).unwrap();
    let gh = world.client(git_env()).build();
    let negative = answer(&gh, &store, &ancestor, &root).unwrap();
    assert_eq!(negative["queries"][0]["contained"], json!(false));
    assert_eq!(negative["acquired_commits"], json!(0));
    assert_eq!(negative["api_body_bytes"], json!(0));
    let tip = host_git(
        &store,
        &["commit-tree", &tree, "-p", &ancestor, "-m", "tip"],
    );
    let positive = answer(&gh, &store, &ancestor, &tip).unwrap();
    assert_eq!(positive["queries"][0]["contained"], json!(true));
    assert_eq!(positive["acquired_commits"], json!(0));
    assert_eq!(host_git(&store, &["show-ref"]), refs);
    assert_eq!(std::fs::read(store.join("config")).unwrap(), config);
    assert!(gh.fx().children_run().iter().all(|step| {
        !step
            .args
            .iter()
            .any(|arg| arg == "fetch" || arg == "clone" || arg == "update-ref")
    }));
}
