// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Production selected-checkout acquisition followed by the real atomic write path.

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{OnceLock, Weak};

struct Fixture {
    world: World,
    remote: PathBuf,
    source: PathBuf,
    before: String,
    after: String,
    historical: String,
    new_blob: String,
}

fn output(dir: &Path, args: &[&str], input: &[u8]) -> String {
    let out = super::raw_blob_tests::git_input(dir, args, Some(input));
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .trim_end_matches('\n')
        .to_owned()
}

fn missing_object(source: &Path, oid: &str) -> bool {
    !super::raw_blob_tests::git_input(source, &["cat-file", "-e", oid], None)
        .status
        .success()
}

fn fixture() -> Fixture {
    fixture_with_history(0)
}

/// One native import, not one process per object. The existing complete-spool
/// admission must handle a history wider than its bounded diagnostic tail.
fn wide_commit(source: &Path, parent: &str, directory: &str, width: usize) -> String {
    use std::io::{Seek as _, Write as _};
    let mut input = format!("commit refs/heads/main\ncommitter fixture <fixture> 1700000000 +0000\ndata 4\nwide\nfrom {parent}\n").into_bytes();
    for n in 0..width {
        let body = format!("{directory} blob {n}\n");
        input.extend_from_slice(
            format!(
                "M 100644 inline {directory}/{n}\ndata {}\n{body}\n",
                body.len()
            )
            .as_bytes(),
        );
    }
    input.extend_from_slice(b"done\n");
    let mut stdin = aterm_tempfile::tempfile().unwrap();
    stdin.write_all(&input).unwrap();
    stdin.rewind().unwrap();
    let mut cmd = aterm_uds::spawnfd::command(host_git_path());
    cmd.args(["fast-import", "--quiet", "--done"])
        .current_dir(source)
        .env_clear()
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_NO_REPLACE_OBJECTS", "1");
    let out = aterm_uds::deadline::output_from_while_process_progressing(
        &mut cmd,
        aterm_uds::spawnfd::Stdio::from(stdin),
        common::HANG,
        "preparing the wide historical-blob fixture",
    )
    .unwrap();
    assert!(out.status.success(), "{out:?}");
    host_git(source, &["rev-parse", "main"])
}

fn fixture_with_history(width: usize) -> Fixture {
    let world = World::canonical("selected-checkout-push-history");
    let remote = bare_repo(world.path(), "remote");
    let upstream = world.path().join("remote-work");
    std::fs::write(upstream.join("payload"), b"original historical blob\n").unwrap();
    host_git(&upstream, &["add", "payload"]);
    host_git(&upstream, &["commit", "--quiet", "-m", "historical A"]);
    let historical = host_git(&upstream, &["rev-parse", "HEAD:payload"]);
    if width != 0 {
        let parent = host_git(&upstream, &["rev-parse", "HEAD"]);
        wide_commit(&upstream, &parent, "old-wide", width);
        // The staged selected tree deliberately omits the newly imported
        // historical subtree, so B's checkout cannot supply those old bodies.
    }
    std::fs::write(upstream.join("payload"), b"selected current blob\n").unwrap();
    host_git(&upstream, &["add", "payload"]);
    host_git(&upstream, &["commit", "--quiet", "-m", "selected B"]);
    host_git(
        &upstream,
        &["push", "--quiet", remote.to_str().unwrap(), "main"],
    );
    host_git(&remote, &["config", "uploadpack.allowFilter", "true"]);
    host_git(
        &remote,
        &["config", "uploadpack.allowReachableSHA1InWant", "true"],
    );
    let before = host_git(&remote, &["rev-parse", "main"]);
    let source = world.path().join("selected-source");
    host_git(
        world.path(),
        &["init", "--quiet", "--template=", source.to_str().unwrap()],
    );
    let gh = world
        .client(git_env())
        .git_remote_for_tests("fixture-dev/app", &remote)
        .build();
    gh.run_git(&GitRun::fetch_ref(
        &common::slug("fixture-dev/app"),
        &source,
        &before,
        "refs/agh/selected",
        true,
        agh::git::FetchScope::SelectedCheckout { cache: None },
    ))
    .expect("actual selected-checkout acquisition");
    assert!(
        missing_object(&source, &historical),
        "historical A body really is omitted"
    );
    host_git(&source, &["checkout", "--quiet", "-b", "main", &before]);
    assert_eq!(
        host_git(&source, &["show", "HEAD:payload"]),
        "selected current blob"
    );
    std::fs::write(source.join("new"), b"new C blob must be strictly checked\n").unwrap();
    host_git(&source, &["add", "new"]);
    host_git(&source, &["commit", "--quiet", "-m", "local C"]);
    let after = host_git(&source, &["rev-parse", "HEAD"]);
    let new_blob = host_git(&source, &["rev-parse", "HEAD:new"]);
    assert!(
        !host_git(
            &source,
            &[
                "rev-list",
                "--objects",
                "--missing=print",
                &after,
                &format!("^{before}")
            ]
        )
        .lines()
        .any(|line| line.starts_with('?')),
        "new delta is already complete"
    );
    Fixture {
        world,
        remote,
        source,
        before,
        after,
        historical,
        new_blob,
    }
}

/// Expand only owned fixture packs so individual corrupt/missing metadata controls
/// do not conflate a missing tree with a removed pack containing many other objects.
fn loosen(source: &Path) {
    let dir = source.join(".git/objects/pack");
    let packs = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "pack"))
        .map(|p| {
            let bytes = std::fs::read(&p).unwrap();
            (p, bytes)
        })
        .collect::<Vec<_>>();
    for (path, bytes) in packs {
        std::fs::remove_file(path.with_extension("idx")).unwrap();
        std::fs::remove_file(&path).unwrap();
        output(source, &["unpack-objects", "-r"], &bytes);
    }
}

fn remove_loose(source: &Path, oid: &str) {
    std::fs::remove_file(source.join(".git/objects").join(&oid[..2]).join(&oid[2..])).unwrap();
}

fn positive() {
    let f = fixture();
    // Unrelated local refs must not widen the remote historical read authority.
    let unrelated_blob = output(
        &f.source,
        &["hash-object", "-w", "--stdin"],
        b"unrelated omitted body\n",
    );
    let tree = output(
        &f.source,
        &["mktree"],
        format!("100644 blob {unrelated_blob}\toutside\n").as_bytes(),
    );
    let commit = host_git(
        &f.source,
        &["commit-tree", &tree, "-m", "unrelated local root"],
    );
    host_git(&f.source, &["update-ref", "refs/heads/unrelated", &commit]);
    remove_loose(&f.source, &unrelated_blob);
    let config = std::fs::read(f.source.join(".git/config")).unwrap();
    let refs = host_git(&f.source, &["show-ref"]);
    let source_objects = host_git(
        &f.source,
        &[
            "cat-file",
            "--batch-all-objects",
            "--batch-check=%(objectname)",
        ],
    );
    let gh = f
        .world
        .client(git_env())
        .git_remote_for_tests("fixture-dev/app", &f.remote)
        .build();
    let slug = common::slug("fixture-dev/app");
    let batch = push_batch(&[("refs/heads/main", &f.before, &f.after)]);
    gh.run_git(&GitRun::push_refs(&slug, &f.source, batch.clone(), true))
        .expect("dry push from actual partial source");
    assert_eq!(
        host_git(&f.remote, &["rev-parse", "main"]),
        f.before,
        "dry run writes nothing"
    );
    journaled_push(&gh, &GitRun::push_refs(&slug, &f.source, batch, false))
        .expect("journaled atomic push with certified private hydration");
    assert_eq!(host_git(&f.remote, &["rev-parse", "main"]), f.after);
    assert_eq!(
        host_git(&f.remote, &["show", "main:new"]),
        "new C blob must be strictly checked"
    );
    assert!(missing_object(&f.source, &f.historical));
    assert!(missing_object(&f.source, &unrelated_blob));
    assert_eq!(std::fs::read(f.source.join(".git/config")).unwrap(), config);
    assert_eq!(host_git(&f.source, &["show-ref"]), refs);
    assert_eq!(
        host_git(
            &f.source,
            &[
                "cat-file",
                "--batch-all-objects",
                "--batch-check=%(objectname)"
            ]
        ),
        source_objects
    );
    let children = gh.fx().children_run();
    let fetches = children
        .iter()
        .filter(|s| s.args.iter().any(|a| a == "fetch"))
        .collect::<Vec<_>>();
    assert_eq!(
        fetches.len(),
        2,
        "one exact private historical read in each independent dry/actual run"
    );
    assert!(fetches.iter().all(
        |s| s.cwd.as_ref().is_some_and(|p| p.ends_with("objects.git"))
            && !s.args.iter().any(|a| a == "--filter=blob:none")
    ));
    assert_eq!(
        children
            .iter()
            .filter(|s| s.args.iter().any(|a| a == "--connectivity-only"))
            .count(),
        2
    );
    assert!(
        children
            .iter()
            .filter(|s| s.args.iter().any(|a| a == "index-pack")
                && s.args.iter().any(|a| a == "--strict"))
            .count()
            >= 6,
        "historical, original metadata and new packs all retain strict verification"
    );
}

fn complete_native_spools_past_one_capture() {
    const WIDTH: usize = 26_000;
    let mut f = fixture_with_history(WIDTH);
    let old_blob = host_git(
        &f.remote,
        &["rev-parse", &format!("{}^:old-wide/25999", f.before)],
    );
    assert!(missing_object(&f.source, &old_blob));
    f.after = wide_commit(&f.source, &f.after, "new-wide", WIDTH);
    let old_large = Arc::new(AtomicBool::new(false));
    let new_large = Arc::new(AtomicBool::new(false));
    let old_seen = Arc::clone(&old_large);
    let new_seen = Arc::clone(&new_large);
    let slot = Arc::new(OnceLock::<Weak<Fx>>::new());
    let observed = Arc::clone(&slot);
    let fx = Arc::new(
        Fx::simulated(live_start()).with_git_child_observer_for_tests(move |span| {
            let fx = observed.get().unwrap().upgrade().unwrap();
            let children = fx.children_run();
            let spec = &children[span.ordinal];
            if !spec.args.iter().any(|a| a == "--missing=print") {
                return;
            }
            let parent = spec.cwd.as_ref().unwrap().parent().unwrap();
            if !spec
                .args
                .iter()
                .any(|a| a.to_string_lossy().starts_with('^'))
                && !spec.args.iter().any(|a| a == "--filter=blob:none")
            {
                let path = parent.join("push-history-owned-missing.records");
                assert!(std::fs::metadata(path).unwrap().len() > 1 << 20);
                old_seen.store(true, Ordering::SeqCst);
            }
            if spec
                .args
                .iter()
                .any(|a| a.to_string_lossy().starts_with('^'))
                && !spec.args.iter().any(|a| a == "--quiet")
            {
                let path = parent.join("push-history-original-new.records");
                assert!(std::fs::metadata(path).unwrap().len() > 1 << 20);
                new_seen.store(true, Ordering::SeqCst);
            }
        }),
    );
    slot.set(Arc::downgrade(&fx)).unwrap();
    let gh = f
        .world
        .client(git_env())
        .git_remote_for_tests("fixture-dev/app", &f.remote)
        .fx(fx)
        .build();
    journaled_push(
        &gh,
        &GitRun::push_refs(
            &common::slug("fixture-dev/app"),
            &f.source,
            push_batch(&[("refs/heads/main", &f.before, &f.after)]),
            false,
        ),
    )
    .expect("complete production inventories larger than one captured output are admitted");
    assert!(
        old_large.load(Ordering::SeqCst) && new_large.load(Ordering::SeqCst),
        "both complete native spools actually exceeded the one-MiB diagnostic capture"
    );
    assert_eq!(host_git(&f.remote, &["rev-parse", "main"]), f.after);
    assert_eq!(
        host_git(&f.remote, &["show", "main:new-wide/25999"]),
        "new-wide blob 25999"
    );
    assert!(missing_object(&f.source, &f.historical) && missing_object(&f.source, &old_blob));
}

fn refuses(fault: &str) {
    let f = fixture();
    let mut before = f.before.clone();
    if matches!(fault, "old-tree" | "missing-tree" | "missing-parent") {
        loosen(&f.source);
    }
    match fault {
        "missing-new" => remove_loose(&f.source, &f.new_blob),
        "corrupt-new" => {
            let substitute = output(
                &f.source,
                &["hash-object", "-w", "--stdin"],
                b"corrupt substitute\n",
            );
            replace_loose(&f.source, &f.new_blob, &substitute);
        }
        "old-tree" => {
            let old = host_git(&f.source, &["rev-parse", &format!("{}^{{tree}}", f.before)]);
            let new = host_git(&f.source, &["rev-parse", "HEAD^{tree}"]);
            replace_loose(&f.source, &old, &new);
        }
        "missing-tree" => {
            let old = host_git(&f.source, &["rev-parse", &format!("{}^{{tree}}", f.before)]);
            remove_loose(&f.source, &old);
        }
        "missing-parent" => remove_loose(&f.source, &f.before),
        "wrong-before" => before = host_git(&f.source, &["rev-parse", &format!("{}^", f.before)]),
        "incomplete"
        | "unexpected-intake"
        | "late-metadata"
        | "early-metadata"
        | "expired"
        | "expired-at-intake-init" => {}
        _ => panic!("unknown fault"),
    }
    let original_source = (fault == "expired-at-intake-init").then(|| {
        (
            std::fs::read(f.source.join(".git/config")).unwrap(),
            host_git(&f.source, &["show-ref"]),
            host_git(
                &f.source,
                &[
                    "cat-file",
                    "--batch-all-objects",
                    "--batch-check=%(objectname)",
                ],
            ),
        )
    });
    let changed = Arc::new(AtomicBool::new(false));
    let happened = Arc::clone(&changed);
    let effect_slot = Arc::new(OnceLock::<Weak<Fx>>::new());
    let observed = Arc::clone(&effect_slot);
    let metadata = Arc::new(std::sync::Mutex::new(None::<PathBuf>));
    let original_metadata = Arc::clone(&metadata);
    let fault_name = fault.to_owned();
    let fx = Arc::new(
        Fx::simulated(live_start()).with_git_child_observer_for_tests(move |span| {
            let fx = observed.get().unwrap().upgrade().unwrap();
            let children = fx.children_run();
            let spec = &children[span.ordinal];
            if spec.args.iter().any(|a| a == "index-pack")
                && spec.args.iter().any(|a| a == "--fsck-objects")
            {
                let mut original = original_metadata.lock().unwrap();
                if original.is_none() {
                    *original = spec
                        .args
                        .last()
                        .map(|value| PathBuf::from(value.as_os_str()));
                    if fault_name == "early-metadata" {
                        let path = original.as_ref().unwrap();
                        let bytes = std::fs::read(path).unwrap();
                        std::fs::remove_file(path).unwrap();
                        std::fs::write(path, bytes).unwrap();
                        happened.store(true, Ordering::SeqCst);
                    }
                }
            }
            if fault_name == "expired"
                && spec.args.iter().any(|a| a == "--missing=print")
                && spec.args.iter().any(|a| a == "--filter=blob:none")
            {
                // Simulated time only: a later walk cannot obtain a new budget.
                fx.sleep(Duration::from_secs(601)).unwrap();
                happened.store(true, Ordering::SeqCst);
            }
            if fault_name == "expired-at-intake-init"
                && span.code == Some(0)
                && spec.args.iter().any(|a| a == "init")
                && spec
                    .args
                    .last()
                    .is_some_and(|p| Path::new(p).ends_with("objects.git"))
            {
                // The real isolated worker exists, but its next remote read
                // must obtain the remaining budget AFTER this successful init.
                fx.sleep(Duration::from_secs(601)).unwrap();
                happened.store(true, Ordering::SeqCst);
            }
            if fault_name == "incomplete"
                && spec.args.iter().any(|a| a == "--missing=print")
                && spec
                    .args
                    .iter()
                    .any(|a| a.to_string_lossy().starts_with('^'))
            {
                let path = spec
                    .cwd
                    .as_ref()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .join("push-history-new-missing.records");
                std::fs::write(path, b"?1234").unwrap();
                happened.store(true, Ordering::SeqCst);
            }
            if fault_name == "unexpected-intake"
                && spec.args.iter().any(|a| a == "fetch")
                && spec
                    .cwd
                    .as_ref()
                    .is_some_and(|p| p.ends_with("objects.git"))
            {
                output(
                    spec.cwd.as_ref().unwrap(),
                    &["hash-object", "-w", "--stdin"],
                    b"unlicensed object\n",
                );
                happened.store(true, Ordering::SeqCst);
            }
            if fault_name == "late-metadata"
                && spec.args.iter().any(|a| a == "index-pack")
                && spec.args.iter().any(|a| a == "--strict")
                && spec
                    .cwd
                    .as_ref()
                    .is_some_and(|p| p.ends_with("objects.git"))
            {
                // Same valid bytes under a replaced physical file: the final pin
                // check must notice AFTER the last private intake completes.
                let path = original_metadata.lock().unwrap().clone().unwrap();
                let body = std::fs::read(&path).unwrap();
                std::fs::remove_file(&path).unwrap();
                std::fs::write(path, body).unwrap();
                happened.store(true, Ordering::SeqCst);
            }
        }),
    );
    effect_slot.set(Arc::downgrade(&fx)).unwrap();
    let gh = f
        .world
        .client(git_env())
        .git_remote_for_tests("fixture-dev/app", &f.remote)
        .fx(fx)
        .build();
    let error = journaled_push(
        &gh,
        &GitRun::push_refs(
            &common::slug("fixture-dev/app"),
            &f.source,
            push_batch(&[("refs/heads/main", &before, &f.after)]),
            false,
        ),
    )
    .expect_err(fault);
    assert_eq!(error.side_effect, SideEffect::None, "{fault}: {error:?}");
    let children = gh.fx().children_run();
    assert!(
        !children.iter().any(|s| s.args.iter().any(|a| a == "push")),
        "{fault} reached writer"
    );
    assert_eq!(
        host_git(&f.remote, &["rev-parse", "main"]),
        f.before,
        "{fault}"
    );
    assert!(
        missing_object(&f.source, &f.historical),
        "{fault}: original source stays partial"
    );
    if matches!(
        fault,
        "incomplete"
            | "unexpected-intake"
            | "late-metadata"
            | "early-metadata"
            | "expired"
            | "expired-at-intake-init"
    ) {
        assert!(
            changed.load(Ordering::SeqCst),
            "{fault}: actual seam reached"
        );
    }
    match fault {
        "wrong-before" => assert_eq!(error.kind, Kind::Conflict),
        "incomplete" => assert!(
            error.why.contains("incomplete object inventory"),
            "{error:?}"
        ),
        "early-metadata" | "late-metadata" => assert!(
            error.why.contains("changed during certification"),
            "{error:?}"
        ),
        "unexpected-intake" => assert!(error.why.contains("unlicensed object"), "{error:?}"),
        _ => {}
    }
    if let Some((config, refs, objects)) = original_source {
        assert_eq!(std::fs::read(f.source.join(".git/config")).unwrap(), config);
        assert_eq!(host_git(&f.source, &["show-ref"]), refs);
        assert_eq!(
            host_git(
                &f.source,
                &[
                    "cat-file",
                    "--batch-all-objects",
                    "--batch-check=%(objectname)",
                ],
            ),
            objects,
            "{fault}: source object membership is unchanged"
        );
        assert_eq!(
            children
                .iter()
                .filter(|s| s.args.iter().any(|a| a == "fetch")
                    && s.cwd.as_ref().is_some_and(|p| p.ends_with("objects.git")))
                .count(),
            0,
            "{fault}: expired preparation must not start a historical worker fetch"
        );
    }
    assert_eq!(
        children
            .iter()
            .filter(|s| s.args.iter().any(|a| a == "fetch"))
            .count(),
        usize::from(matches!(fault, "unexpected-intake" | "late-metadata")),
        "{fault}: no unauthorized historical read"
    );
}

#[test]
fn selected_checkout_push_certifies_only_omitted_remote_blobs() {
    positive();
    complete_native_spools_past_one_capture();
    for fault in [
        "missing-new",
        "corrupt-new",
        "old-tree",
        "missing-tree",
        "missing-parent",
        "wrong-before",
        "incomplete",
        "unexpected-intake",
        "late-metadata",
        "early-metadata",
        "expired",
        "expired-at-intake-init",
    ] {
        refuses(fault);
    }
}
