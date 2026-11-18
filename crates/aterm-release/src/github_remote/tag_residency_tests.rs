// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use super::*;

#[test]
fn fresh_remote_rechecks_annotated_tag_residency_without_duplicate_import() {
    let root = aterm_tempfile::tempdir().unwrap();
    let bare = repo(root.path(), "remote.git", true);
    let source = repo(root.path(), "source", false);
    let main = commit(&source, "published source\n");
    git(&source, &["tag", "-a", "v1", "-m", "first tag"]);
    git(
        &source,
        &["push", "-q", bare.to_str().unwrap(), "main", "--tags"],
    );
    let target = repo(root.path(), "target", false);
    let head = commit(&target, "owned checkout\n");
    git(
        &target,
        &[
            "config",
            "remote.origin.url",
            "https://github.com/fixture/app.git",
        ],
    );
    let effects = Arc::new(agh::Fx::simulated(agh::Fx::live().wall_secs().unwrap()));
    let client = GithubBuilder::new(Router::empty())
        .fx(effects.clone())
        .git_remote_for_tests("fixture/app", &bare)
        .build();
    let remote = Remote::new(client, Slug::parse("fixture/app").unwrap(), &target).unwrap();
    let first = remote.observe_origin().unwrap();
    assert_eq!(first.main, main);
    assert!(
        effects
            .children_run()
            .iter()
            .any(|child| child.args.iter().any(|arg| arg == "fetch"))
    );

    let fresh = Remote::new(remote.client.clone(), remote.slug.clone(), &target).unwrap();
    let began = effects.children_run().len();
    let second = fresh.observe_origin().unwrap();
    assert_eq!((second.main, second.tags), (first.main, first.tags));
    let children = effects.children_run();
    let current = &children[began..];
    assert_eq!(
        current
            .iter()
            .filter(|child| child.args.iter().any(|arg| arg == "ls-remote"))
            .count(),
        2,
        "both main and tags are freshly advertised"
    );
    assert!(
        current
            .iter()
            .any(|child| child.args.iter().any(|arg| arg == "--git-common-dir")),
        "the native source is admitted again"
    );
    assert!(
        current
            .iter()
            .any(|child| child.args.iter().any(|arg| arg == "--batch")),
        "raw tag bodies are independently read and hashed again"
    );
    assert!(
        !current
            .iter()
            .any(|child| child.args.iter().any(|arg| matches!(
                arg.to_str(),
                Some("init" | "fetch" | "pack-objects" | "index-pack")
            ))),
        "complete resident objects need no data transfer/import"
    );
    assert_eq!(git(&target, &["rev-parse", "HEAD"]), head);

    // An altered raw tag with the same peeled commit is a new snapshot root.
    git(&source, &["tag", "-fa", "v1", "-m", "changed tag", &main]);
    let changed = git(&source, &["rev-parse", "v1"]);
    git(
        &source,
        &[
            "push",
            "-q",
            "--force",
            bare.to_str().unwrap(),
            "refs/tags/v1",
        ],
    );
    let again = Remote::new(remote.client.clone(), remote.slug.clone(), &target).unwrap();
    let began = effects.children_run().len();
    let observed = again.observe_origin().unwrap();
    assert_eq!(
        parse_tags(&observed.tags).unwrap().get("refs/tags/v1"),
        Some(&changed)
    );
    assert_eq!(observed.main, main);
    assert!(
        effects.children_run()[began..]
            .iter()
            .any(|child| child.args.iter().any(|arg| arg == "fetch")),
        "the new raw tag was absent locally"
    );

    // Local DATA cannot supply a current advertisement or override source policy.
    let offline = root.path().join("offline.git");
    std::fs::rename(&bare, &offline).unwrap();
    assert!(again.observe_origin().is_err());
    std::fs::rename(&offline, &bare).unwrap();
    let config = std::fs::read(target.join(".git/config")).unwrap();
    git(
        &target,
        &[
            "config",
            "core.hooksPath",
            root.path().join("unused-hooks").to_str().unwrap(),
        ],
    );
    let began = effects.children_run().len();
    assert!(again.observe_origin().is_err());
    // Advertisements use the captured repository, while local objects require
    // fresh source admission before either residency proof or cold intake.
    assert!(
        effects.children_run()[began..].iter().all(|child| {
            !child.args.iter().any(|arg| {
                matches!(
                    arg.to_str(),
                    Some(
                        "cat-file"
                            | "rev-list"
                            | "hash-object"
                            | "init"
                            | "fetch"
                            | "pack-objects"
                            | "index-pack"
                    )
                )
            })
        }),
        "changed source policy must refuse before local object proof or intake"
    );
    std::fs::write(target.join(".git/config"), config).unwrap();
}
