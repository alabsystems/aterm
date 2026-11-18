// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use super::*;

struct Fixture {
    world: World,
    remote: PathBuf,
    source: PathBuf,
    commit: String,
    tag: String,
    historical_blob: String,
}

fn fixture(name: &str) -> Fixture {
    let world = World::canonical(name);
    let remote = bare_repo(world.path(), "app");
    let source = world.path().join("app-work");
    std::fs::write(source.join("payload"), "historical body\n").unwrap();
    host_git(&source, &["add", "payload"]);
    host_git(&source, &["commit", "--quiet", "-m", "historical body"]);
    let historical_blob = host_git(&source, &["rev-parse", "HEAD:payload"]);
    std::fs::write(source.join("payload"), "selected body\n").unwrap();
    host_git(&source, &["commit", "--quiet", "-am", "selected body"]);
    let commit = host_git(&source, &["rev-parse", "HEAD"]);
    host_git(&source, &["tag", "-a", "resident", "-m", "resident tag"]);
    let tag = host_git(&source, &["rev-parse", "resident"]);
    host_git(
        &source,
        &[
            "push",
            "--quiet",
            remote.to_str().unwrap(),
            "main",
            "--tags",
        ],
    );
    Fixture {
        world,
        remote,
        source,
        commit,
        tag,
        historical_blob,
    }
}

fn client(f: &Fixture) -> Github {
    f.world
        .client(git_env())
        .git_remote_for_tests("fixture-dev/app", &f.remote)
        .build()
}

fn run(f: &Fixture, gh: &Github, tag: &str) -> agh::Result<agh::git::GitOut> {
    gh.run_git(&GitRun::fetch_batch(
        &common::slug("fixture-dev/app"),
        &f.source,
        FetchBatch::objects(vec![f.commit.clone(), tag.to_owned()]).unwrap(),
        false,
    ))
}

fn data_steps(gh: &Github) -> Vec<ChildSpec> {
    gh.fx()
        .children_run()
        .into_iter()
        .filter(|spec| {
            spec.args.iter().any(|arg| {
                matches!(
                    arg.to_str(),
                    Some("init" | "fetch" | "pack-objects" | "index-pack")
                )
            })
        })
        .collect()
}

fn tag_body(id: &str, kind: &str, message: &[u8]) -> Vec<u8> {
    let mut raw = format!("object {id}\ntype {kind}\ntag resident\ntagger Fixture <fixture@example.invalid> 1700000000 +0000\n\n").into_bytes();
    raw.extend_from_slice(message);
    raw
}

fn put_tag(repo: &Path, raw: &[u8]) -> String {
    let out = super::raw_blob_tests::git_input(
        repo,
        &["hash-object", "--literally", "-t", "tag", "-w", "--stdin"],
        Some(raw),
    );
    assert!(out.status.success(), "{out:?}");
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

#[test]
fn literal_tag_residency_reuses_nested_raw_roots_without_data_children() {
    let f = fixture("local-tag-resident");
    let nested = put_tag(&f.source, &tag_body(&f.tag, "tag", &[0xff, 0xfe, b'\n']));
    let gh = client(&f);
    let refs = host_git(&f.source, &["show-ref"]);
    let config = std::fs::read(f.source.join(".git/config")).unwrap();
    run(&f, &gh, &nested).unwrap();
    assert!(
        data_steps(&gh).is_empty(),
        "resident raw chains need no transfer/import: {:?}",
        data_steps(&gh)
    );
    assert_eq!(host_git(&f.source, &["show-ref"]), refs);
    assert_eq!(std::fs::read(f.source.join(".git/config")).unwrap(), config);
    assert!(!f.source.join(".git/objects/info/alternates").exists());
}

#[test]
fn literal_tag_residency_refuses_forgery_and_uses_cold_header_validation() {
    for broken in ["forged", "declared-type", "duplicate", "unknown-header"] {
        let f = fixture(&format!("local-tag-{broken}"));
        let tag = if broken == "forged" {
            let replacement = put_tag(
                &f.source,
                &tag_body(&f.commit, "commit", b"different message\n"),
            );
            replace_loose(&f.source, &f.tag, &replacement);
            f.tag.clone()
        } else {
            let mut raw = tag_body(
                &f.commit,
                if broken == "declared-type" {
                    "tag"
                } else {
                    "commit"
                },
                b"message\n",
            );
            if broken == "duplicate" {
                let at = raw.windows(4).position(|part| part == b"tag ").unwrap();
                raw.splice(at..at, b"type commit\n".iter().copied());
            } else if broken == "unknown-header" {
                let at = raw.windows(7).position(|part| part == b"tagger ").unwrap();
                raw.splice(at..at + 7, b"broken ".iter().copied());
            }
            put_tag(&f.source, &raw)
        };
        let gh = client(&f);
        let result = run(&f, &gh, &tag);
        if broken == "unknown-header" {
            // Git accepts this form through the existing cold path. Advice
            // must decline it without adding a new acceptance restriction.
            result.expect("an unsupported header retains its ordinary cold acceptance");
        } else {
            assert!(result.is_err(), "{broken} cannot pass cold validation");
        }
        assert!(
            !data_steps(&gh).is_empty(),
            "{broken} must use the ordinary cold path"
        );
    }
}

#[test]
fn literal_tag_residency_keeps_valid_cold_forms_and_depth_size_bounds() {
    for form in ["blob", "tree", "deep", "large"] {
        let f = fixture(&format!("local-tag-cold-{form}"));
        let tag = match form {
            "blob" | "tree" => {
                let id = host_git(
                    &f.source,
                    &[
                        "rev-parse",
                        if form == "blob" {
                            "HEAD:payload"
                        } else {
                            "HEAD^{tree}"
                        },
                    ],
                );
                put_tag(&f.source, &tag_body(&id, form, b"valid noncommit tag\n"))
            }
            "large" => put_tag(
                &f.source,
                &tag_body(&f.commit, "commit", &vec![b'x'; 256 * 1024 + 1]),
            ),
            _ => {
                let mut id = f.commit.clone();
                for n in 0..34 {
                    id = put_tag(
                        &f.source,
                        &tag_body(
                            &id,
                            if n == 0 { "commit" } else { "tag" },
                            format!("level {n}\n").as_bytes(),
                        ),
                    );
                }
                id
            }
        };
        host_git(&f.source, &["update-ref", "refs/tags/cold", &tag]);
        host_git(
            &f.source,
            &[
                "push",
                "--quiet",
                f.remote.to_str().unwrap(),
                "refs/tags/cold",
            ],
        );
        let gh = client(&f);
        run(&f, &gh, &tag)
            .unwrap_or_else(|error| panic!("valid {form} keeps its cold acceptance: {error:?}"));
        assert!(!data_steps(&gh).is_empty(), "{form} takes the cold import");
    }
}

#[test]
fn literal_tag_residency_does_not_hide_missing_historical_blobs_behind_cutoffs() {
    for cutoff in ["none", "graft", "replace", "shallow"] {
        let f = fixture(&format!("local-tag-missing-{cutoff}"));
        let object = f
            .source
            .join(".git/objects")
            .join(&f.historical_blob[..2])
            .join(&f.historical_blob[2..]);
        std::fs::remove_file(&object).unwrap();
        match cutoff {
            "graft" => std::fs::write(f.source.join(".git/info/grafts"), format!("{}\n", f.commit))
                .unwrap(),
            "replace" => {
                let tree = host_git(&f.source, &["rev-parse", "HEAD^{tree}"]);
                let detached = host_git(&f.source, &["commit-tree", &tree, "-m", "false root"]);
                host_git(&f.source, &["replace", &f.commit, &detached]);
            }
            "shallow" => {
                std::fs::write(f.source.join(".git/shallow"), format!("{}\n", f.commit)).unwrap()
            }
            _ => {}
        }
        let gh = client(&f);
        run(&f, &gh, &f.tag).unwrap();
        assert!(
            !data_steps(&gh).is_empty(),
            "{cutoff}: incomplete history cannot be a cache hit"
        );
        assert!(
            super::raw_blob_tests::git_input(
                &f.source,
                &["cat-file", "-e", &f.historical_blob],
                None
            )
            .status
            .success()
        );
    }
}

#[test]
fn literal_tag_residency_cannot_accept_missing_graph_members() {
    for missing in ["raw-tag", "tag-target", "parent", "tree"] {
        let f = fixture(&format!("local-tag-missing-{missing}"));
        let id = match missing {
            "raw-tag" => f.tag.clone(),
            "tag-target" => f.commit.clone(),
            "parent" => host_git(&f.source, &["rev-parse", "HEAD^"]),
            _ => host_git(&f.source, &["rev-parse", "HEAD^{tree}"]),
        };
        std::fs::remove_file(f.source.join(".git/objects").join(&id[..2]).join(&id[2..])).unwrap();
        let gh = client(&f);
        let result = run(&f, &gh, &f.tag);
        assert!(
            result.is_err() || !data_steps(&gh).is_empty(),
            "{missing}: a missing original graph member cannot be a successful local cache hit"
        );
    }
}

#[test]
fn literal_tag_residency_shared_suffix_cannot_bypass_the_depth_bound() {
    let f = fixture("local-tag-shared-depth");
    let mut shared = f.commit.clone();
    for level in 0..32 {
        shared = put_tag(
            &f.source,
            &tag_body(
                &shared,
                if level == 0 { "commit" } else { "tag" },
                format!("shared {level}\n").as_bytes(),
            ),
        );
    }
    let extra = put_tag(
        &f.source,
        &tag_body(&shared, "tag", b"one level beyond the shared root\n"),
    );
    for (name, id) in [("refs/tags/shared", &shared), ("refs/tags/extra", &extra)] {
        host_git(&f.source, &["update-ref", name, id]);
        host_git(
            &f.source,
            &["push", "--quiet", f.remote.to_str().unwrap(), name],
        );
    }
    let gh = client(&f);
    run(&f, &gh, &shared).unwrap();
    assert!(
        data_steps(&gh).is_empty(),
        "the bounded shared suffix is resident"
    );
    gh.run_git(&GitRun::fetch_batch(
        &common::slug("fixture-dev/app"),
        &f.source,
        FetchBatch::objects(vec![shared, extra]).unwrap(),
        false,
    ))
    .unwrap();
    assert!(
        !data_steps(&gh).is_empty(),
        "a separately seeded shared root cannot shorten the other root's depth"
    );
}

#[test]
fn literal_tag_residency_child_failure_propagates_without_a_cold_retry() {
    let f = fixture("local-tag-child-failure");
    let probe = client(&f);
    run(&f, &probe, &f.tag).unwrap();
    let spec = probe
        .fx()
        .children_run()
        .into_iter()
        .find(|spec| spec.args.iter().any(|arg| arg == "--batch"))
        .unwrap();
    for code in [None, Some(-1), Some(2), Some(128)] {
        let fx = Fx::simulated(live_start()).with_git_intercept_once_for_tests(
            spec.program.clone(),
            spec.args.clone(),
            || {},
            move |result| {
                if let Some(code) = code {
                    result.as_mut().unwrap().code = Some(code);
                } else {
                    *result = Err(agh::Error::could_not_run(
                        "injected owned-child I/O failure",
                    ));
                }
            },
        );
        let gh = f
            .world
            .client(git_env())
            .fx(Arc::new(fx))
            .git_remote_for_tests("fixture-dev/app", &f.remote)
            .build();
        assert!(run(&f, &gh, &f.tag).is_err(), "{code:?}");
        assert!(
            data_steps(&gh).is_empty(),
            "failed child cannot request a new cold attempt: {code:?}"
        );
    }
}

#[test]
fn literal_tag_residency_native_hash_authority_and_owned_files_are_not_bypassed() {
    let f = fixture("local-tag-native-hash");
    let second = put_tag(
        &f.source,
        &tag_body(&f.commit, "commit", b"second valid tag\n"),
    );
    host_git(&f.source, &["update-ref", "refs/tags/second", &second]);
    host_git(
        &f.source,
        &[
            "push",
            "--quiet",
            f.remote.to_str().unwrap(),
            "refs/tags/second",
        ],
    );
    let batch = FetchBatch::objects(vec![f.commit.clone(), f.tag.clone(), second]).unwrap();
    let operation = GitRun::fetch_batch(&common::slug("fixture-dev/app"), &f.source, batch, false);
    let probe = client(&f);
    probe.run_git(&operation).unwrap();
    let hashes = probe
        .fx()
        .children_run()
        .into_iter()
        .filter(|spec| spec.args.iter().any(|arg| arg == "--stdin-paths"))
        .collect::<Vec<_>>();
    assert_eq!(
        hashes.len(),
        2,
        "two tag bodies share one native hash child; endpoint commit uses the other"
    );
    assert!(hashes.iter().all(|spec| {
        !spec
            .args
            .iter()
            .any(|arg| arg == "-w" || arg == "--literally")
    }));
    assert!(
        hashes
            .iter()
            .all(|spec| spec.cwd.as_ref().is_some_and(|cwd| !cwd.exists())),
        "private body files retire only after the checked child settles"
    );
    let spec = &hashes[0];
    for fault in [
        "reject",
        "io",
        "deadline",
        "cancel",
        "wrong",
        "truncated",
        "reordered",
        "saturated",
    ] {
        let fx = Fx::simulated(live_start()).with_git_intercept_once_for_tests(
            spec.program.clone(),
            spec.args.clone(),
            || {},
            move |result| match fault {
                "io" => {
                    *result = Err(agh::Error::could_not_run(
                        "injected owned hash child I/O failure",
                    ))
                }
                "deadline" => {
                    *result = Err(agh::Error::could_not_run(
                        "injected owned hash child deadline expired",
                    ))
                }
                "cancel" => {
                    *result = Err(agh::Error::new(
                        Kind::Interrupted,
                        "native Git operation interrupted",
                    ))
                }
                "reject" => result.as_mut().unwrap().code = Some(128),
                "wrong" => {
                    result.as_mut().unwrap().stdout =
                        format!("{}\n{}\n", "0".repeat(40), "1".repeat(40)).into_bytes()
                }
                "truncated" => {
                    result.as_mut().unwrap().stdout.pop();
                }
                "reordered" => {
                    let out = result.as_mut().unwrap();
                    let text = std::str::from_utf8(&out.stdout).unwrap();
                    let rows = text.lines().collect::<Vec<_>>();
                    assert_eq!(rows.len(), 2);
                    out.stdout = format!("{}\n{}\n", rows[1], rows[0]).into_bytes();
                }
                _ => result.as_mut().unwrap().stdout = vec![b'x'; 1024 * 1024],
            },
        );
        let gh = f
            .world
            .client(git_env())
            .fx(Arc::new(fx))
            .git_remote_for_tests("fixture-dev/app", &f.remote)
            .build();
        let result = gh.run_git(&operation);
        if matches!(fault, "reject" | "io" | "deadline" | "cancel") {
            assert!(result.is_err(), "{fault}: native authority failed");
            assert!(
                data_steps(&gh).is_empty(),
                "{fault}: a failed native child cannot request a cold retry"
            );
            if fault == "cancel" {
                assert_eq!(result.unwrap_err().kind, Kind::Interrupted);
            }
        } else {
            result.unwrap();
            assert!(
                !data_steps(&gh).is_empty(),
                "{fault}: wrong/incomplete native bindings cannot prove residency"
            );
        }
        assert!(
            gh.fx()
                .children_run()
                .iter()
                .filter(|spec| spec.args.iter().any(|arg| arg == "--stdin-paths"))
                .all(|spec| spec.cwd.as_ref().is_some_and(|cwd| !cwd.exists())),
            "{fault}: private hash inputs remain owned through settlement and are then removed"
        );
    }
}
