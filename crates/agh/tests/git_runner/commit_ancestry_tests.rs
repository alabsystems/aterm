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

struct FilteredStore {
    donor: PathBuf,
    store: PathBuf,
    first: String,
    tip: String,
    retired: String,
}

fn filtered_store(world: &World) -> FilteredStore {
    let donor = bare_repo(world.path(), "filtered-donor");
    let work = world.path().join("filtered-donor-work");
    let first = host_git(&donor, &["rev-parse", "main"]);
    std::fs::write(work.join("retired"), vec![0xa5; 64 * 1024]).unwrap();
    host_git(&work, &["add", "retired"]);
    host_git(&work, &["commit", "--quiet", "-m", "historical blob"]);
    let retired = host_git(&work, &["rev-parse", "HEAD:retired"]);
    host_git(&work, &["rm", "--quiet", "retired"]);
    host_git(&work, &["commit", "--quiet", "-m", "current source"]);
    host_git(&work, &["push", "--quiet", donor.to_str().unwrap(), "main"]);
    host_git(&donor, &["config", "uploadpack.allowFilter", "true"]);
    let tip = host_git(&donor, &["rev-parse", "main"]);
    let store = world.path().join("filtered-store.git");
    // A file URL uses real upload-pack, rather than local hardlink cloning.
    host_git(
        world.path(),
        &[
            "clone",
            "--quiet",
            "--bare",
            "--filter=blob:none",
            &format!("file://{}", donor.display()),
            store.to_str().unwrap(),
        ],
    );
    assert_eq!(
        host_git(&store, &["config", "remote.origin.promisor"]),
        "true"
    );
    assert_eq!(
        host_git(&store, &["config", "remote.origin.partialclonefilter"]),
        "blob:none"
    );
    assert!(!local_object(&store, &retired));
    FilteredStore {
        donor,
        store,
        first,
        tip,
        retired,
    }
}

fn local_object(store: &Path, id: &str) -> bool {
    let mut command = aterm_uds::spawnfd::command(host_git_path());
    command
        .args([
            "-C",
            store.to_str().unwrap(),
            "-c",
            "protocol.allow=never",
            "cat-file",
            "-e",
            id,
        ])
        .env_clear()
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_NO_LAZY_FETCH", "1");
    aterm_uds::deadline::output_until(&mut command, std::time::Instant::now() + common::HANG)
        .unwrap()
        .status
        .success()
}

#[test]
fn real_filtered_clones_allow_only_immutable_commit_reads_and_keep_omitted_blobs_missing() {
    let world = World::canonical("commit-filtered-local");
    let fixture = filtered_store(&world);
    let full = world.path().join("full-control.git");
    host_git(
        world.path(),
        &[
            "clone",
            "--quiet",
            "--bare",
            &format!("file://{}", fixture.donor.display()),
            full.to_str().unwrap(),
        ],
    );
    assert!(
        local_object(&full, &fixture.retired),
        "unfiltered control must contain the exact retired blob"
    );
    let tree = host_git(&fixture.store, &["rev-parse", "main^{tree}"]);
    let unrelated = host_git(&fixture.store, &["commit-tree", &tree, "-m", "unrelated"]);
    let refs = host_git(&fixture.store, &["show-ref"]);
    let config = std::fs::read(fixture.store.join("config")).unwrap();
    std::fs::write(fixture.store.join("FETCH_HEAD"), b"original fetch head\n").unwrap();
    let gh = world
        .client(git_env())
        .git_remote_for_tests("fixture-dev/app", &fixture.donor)
        .build();
    for (ancestor, contained) in [(&fixture.first, true), (&unrelated, false)] {
        let result = answer(&gh, &fixture.store, ancestor, &fixture.tip).unwrap();
        assert_eq!(result["queries"][0]["contained"], json!(contained));
        assert_eq!(result["acquired_commits"], json!(0));
        assert_eq!(result["refs_written"], json!(false));
        assert_eq!(result["checkout_complete"], json!(false));
    }
    let transport = gh.run_git(&GitRun::fetch_main(
        &common::slug("fixture-dev/app"),
        &fixture.store,
        "refs/agh/transport-control",
    ));
    let error = transport.expect_err("commit-read metadata cannot authorize Git transport");
    assert_eq!(error.kind, Kind::Refused);
    assert!(matches!(error.sub, Sub::GitConfig { .. }));
    assert_eq!(host_git(&fixture.store, &["show-ref"]), refs);
    assert_eq!(std::fs::read(fixture.store.join("config")).unwrap(), config);
    assert_eq!(
        std::fs::read(fixture.store.join("FETCH_HEAD")).unwrap(),
        b"original fetch head\n"
    );
    assert!(!local_object(&fixture.store, &fixture.retired));
    assert!(
        world
            .fake
            .seen()
            .iter()
            .all(|call| !call.path_and_query.contains("/git/commits/"))
    );
}

#[test]
fn filtered_missing_commits_are_acquired_as_exact_api_bytes_without_promisor_transport() {
    let world = World::canonical("commit-filtered-acquire");
    let fixture = filtered_store(&world);
    let alternate = bare_repo(world.path(), "auxiliary");
    let alternate_bytes = format!("{}/objects\n", alternate.display());
    std::fs::write(
        fixture.store.join("objects/info/alternates"),
        &alternate_bytes,
    )
    .unwrap();
    let input = world.path().join("future-blob");
    std::fs::write(&input, vec![0x5a; 64 * 1024]).unwrap();
    let blob = host_git(
        &fixture.donor,
        &["hash-object", "-w", input.to_str().unwrap()],
    );
    let mut tree_bytes = b"100644 future\0".to_vec();
    for pair in blob.as_bytes().as_chunks::<2>().0 {
        tree_bytes.push(u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap());
    }
    let input = world.path().join("future-tree");
    std::fs::write(&input, tree_bytes).unwrap();
    let tree = host_git(
        &fixture.donor,
        &["hash-object", "-w", "-t", "tree", input.to_str().unwrap()],
    );
    let middle = host_git(
        &fixture.donor,
        &["commit-tree", &tree, "-p", &fixture.tip, "-m", "middle"],
    );
    let tip = host_git(
        &fixture.donor,
        &["commit-tree", &tree, "-p", &middle, "-m", "future"],
    );
    assert!(!local_object(&fixture.store, &tip));
    api_commit(&world, &fixture.donor, &middle);
    api_commit(&world, &fixture.donor, &tip);
    let config = std::fs::read(fixture.store.join("config")).unwrap();
    let refs = host_git(&fixture.store, &["show-ref"]);
    std::fs::write(fixture.store.join("FETCH_HEAD"), b"original fetch head\n").unwrap();
    let gh = world.client(git_env()).build();
    let result = answer(&gh, &fixture.store, &fixture.first, &tip).unwrap();
    assert_eq!(result["queries"][0]["contained"], json!(true));
    assert_eq!(result["acquired_commits"], json!(2));
    assert!(local_object(&fixture.store, &middle));
    assert!(local_object(&fixture.store, &tip));
    for id in [&tree, &blob, &fixture.retired] {
        assert!(!local_object(&fixture.store, id));
    }
    assert_eq!(std::fs::read(fixture.store.join("config")).unwrap(), config);
    assert_eq!(
        std::fs::read(fixture.store.join("FETCH_HEAD")).unwrap(),
        b"original fetch head\n"
    );
    assert_eq!(host_git(&fixture.store, &["show-ref"]), refs);
    assert_eq!(
        std::fs::read(fixture.store.join("objects/info/alternates")).unwrap(),
        alternate_bytes.as_bytes()
    );
    let reader_children = gh
        .fx()
        .children_run()
        .into_iter()
        .filter(|child| {
            child
                .args
                .iter()
                .any(|arg| arg == "core.commitGraph=false" || arg == "count-objects")
        })
        .collect::<Vec<_>>();
    assert!(!reader_children.is_empty());
    assert!(
        reader_children
            .iter()
            .any(|child| child.args.iter().any(|arg| arg == "count-objects"))
    );
    assert!(reader_children.iter().all(|child| {
        child
            .env
            .iter()
            .any(|(name, value)| name == "GIT_NO_LAZY_FETCH" && value == "1")
    }));
}

#[test]
fn filtered_missing_parent_and_wrong_api_identity_cannot_become_negative_evidence() {
    let world = World::canonical("commit-filtered-missing");
    let fixture = filtered_store(&world);
    let tree = host_git(&fixture.donor, &["rev-parse", "main^{tree}"]);
    let parent = host_git(
        &fixture.donor,
        &[
            "commit-tree",
            &tree,
            "-p",
            &fixture.tip,
            "-m",
            "missing parent",
        ],
    );
    let raw = format!(
        "tree {tree}\nparent {parent}\nauthor fixture <fixture> 1700000000 +0000\ncommitter fixture <fixture> 1700000000 +0000\n\ngap\n"
    );
    let input = world.path().join("gap");
    std::fs::write(&input, raw).unwrap();
    let tip = host_git(
        &fixture.store,
        &["hash-object", "-w", "-t", "commit", input.to_str().unwrap()],
    );
    let unrelated = host_git(&fixture.store, &["commit-tree", &tree, "-m", "unrelated"]);
    assert!(!local_object(&fixture.store, &parent));
    world.fake.fault_always(
        Match::api(&format!("/repos/fixture-dev/app/git/commits/{parent}")),
        Fault::Status {
            status: 200,
            body: format!("{{\"sha\":\"{}\"}}", fixture.first),
            headers: Vec::new(),
        },
    );
    let gh = world.client(git_env()).build();
    assert!(answer(&gh, &fixture.store, &unrelated, &tip).is_err());
    assert!(!local_object(&fixture.store, &parent));
    assert!(!local_object(&fixture.store, &fixture.retired));
    assert!(gh.fx().children_run().iter().all(|child| {
        !child
            .args
            .iter()
            .any(|arg| arg == "fetch" || arg == "update-ref")
    }));
}

#[test]
fn a_missing_promised_blob_cannot_be_fetched_or_accepted_as_a_commit() {
    let world = World::canonical("commit-filtered-promised-blob");
    let fixture = filtered_store(&world);
    let api = format!("/repos/fixture-dev/app/git/commits/{}", fixture.retired);
    world.fake.fault_always(
        Match::api(&api),
        Fault::Status {
            status: 200,
            body: format!("{{\"sha\":\"{}\",\"type\":\"blob\"}}", fixture.retired),
            headers: Vec::new(),
        },
    );
    let requested = CommitRead::new(CommitReadInput {
        commits: vec![fixture.retired.clone()],
        queries: Vec::new(),
    })
    .unwrap();
    let gh = world.client(git_env()).build();
    gh.run_git(&GitRun::commit_read(
        &common::slug("fixture-dev/app"),
        &fixture.store,
        requested,
        true,
    ))
    .expect_err("a missing promised blob is not an authenticated commit");
    assert!(!local_object(&fixture.store, &fixture.retired));
    assert!(
        world
            .fake
            .seen()
            .iter()
            .any(|call| call.path_and_query == api)
    );
}

#[test]
fn filtered_metadata_never_admits_duplicate_or_other_guarded_source_configuration() {
    let world = World::canonical("commit-filtered-config");
    let fixture = filtered_store(&world);
    let config = std::fs::read(fixture.store.join("config")).unwrap();
    let gh = world.client(git_env()).build();
    for added in [
        "[remote \"origin\"]\npromisor = true\n",
        "[remote \"origin\"]\npartialclonefilter = tree:0\n",
        "[remote \"origin\"]\nproxy = https://elsewhere.invalid/\n",
        "[core]\nhooksPath = /untrusted\n",
        "[core]\nalternateRefsCommand = untrusted\n",
        "[extensions]\npartialClone = origin\n",
        "[http]\nsslVerify = false\n",
        "[include]\npath = /untrusted\n",
    ] {
        let bytes = [config.as_slice(), added.as_bytes()].concat();
        std::fs::write(fixture.store.join("config"), bytes).unwrap();
        let error = answer(&gh, &fixture.store, &fixture.first, &fixture.tip)
            .expect_err("filter metadata cannot admit unsafe configuration");
        assert_eq!(error.kind, Kind::Refused, "{added}");
        assert!(matches!(error.sub, Sub::GitConfig { .. }), "{added}");
        std::fs::write(fixture.store.join("config"), &config).unwrap();
    }
    assert!(!local_object(&fixture.store, &fixture.retired));
    assert!(
        world
            .fake
            .seen()
            .iter()
            .all(|call| !call.path_and_query.contains("/git/commits/"))
    );
}
