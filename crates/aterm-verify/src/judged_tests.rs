// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn history_keeps_cancelled_attempts_replaced_by_a_verdict() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let root = tmp.path();
    git(root, &["init", "--quiet"]).unwrap();
    git(root, &["config", "user.name", "Andrew Yates"]).unwrap();
    git(root, &["config", "user.email", "test@example.invalid"]).unwrap();
    git(root, &["remote", "add", "origin", "."]).unwrap();
    let now = aterm_time::unix_secs().unwrap();
    let mut row = Record {
        commit: "head".into(),
        tree: "tree".into(),
        host: "host".into(),
        verifier: String::new(),
        controller: String::new(),
        inputs: String::new(),
        token: "first".into(),
        state: "judging".into(),
        cause: String::new(),
        started: now,
        updated: now,
        lease_until: now + LEASE,
        rows: vec![],
    };
    let mut parent = None;
    for (token, state, delta) in [
        ("first", "judging", 0),
        ("first", "cancelled", 1),
        ("second", "judging", 2),
        ("second", "judging", 3),
        ("second", "pass", 4),
    ] {
        row.token = token.into();
        row.state = state.into();
        row.updated = now + delta;
        assert!(append(root, "origin", parent.as_deref(), &row, deadline()).unwrap());
        parent = head(root, "origin", deadline()).unwrap();
    }
    let head = parent.unwrap();
    let attempts = history(root, &head, now).unwrap();
    assert_eq!(
        attempts.len(),
        2,
        "heartbeats are not attempts; a retry does not erase cancellation"
    );
    assert!(
        attempts
            .iter()
            .any(|r| r.token == "first" && r.state == "cancelled")
    );
    assert!(
        attempts
            .iter()
            .any(|r| r.token == "second" && r.state == "pass")
    );
    assert_eq!(
        records(root, Some(&head), deadline()).unwrap().len(),
        1,
        "negative control: the tree snapshot really has overwritten the cancellation"
    );
    assert_eq!(history(root, &head, now + 2).unwrap().len(), 1);
}

#[cfg(unix)]
#[test]
fn renewal_interrupts_abort_only_scoped_journal_work() {
    const CHILD: &str = "ATERM_VERIFY_RENEWAL_INTERRUPT_CHILD";
    const TEST: &str = "judged::tests::renewal_interrupts_abort_only_scoped_journal_work";
    if std::env::var_os(CHILD).is_some() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "--quiet"]).unwrap();
        git(root, &["init", "--bare", "--quiet", "remote"]).unwrap();
        git(root, &["remote", "add", "origin", "remote"]).unwrap();
        let watch = aterm_toolchain::interrupt::Watch::enter();
        // SAFETY: raise sends this valid signal to this isolated child test process;
        // its live Watch owns the disposition and retains it for the controller.
        #[expect(
            clippy::disallowed_methods,
            reason = "this isolated child catches non-core SIGTERM in its live Watch to test journal cancellation; it must not die by SIGKILL"
        )]
        let raised = unsafe { libc::raise(libc::SIGTERM) };
        assert_eq!(raised, 0);
        let stopped = interruptible(|| output(&mut command(root, &["--version"]), None));
        assert!(stopped.unwrap_err().contains("interrupted"));
        let stopped_remote = interruptible(|| head(root, "origin", deadline()));
        assert!(stopped_remote.unwrap_err().contains("interrupted"));
        assert_eq!(watch.caught(), Some(libc::SIGTERM));
        // The terminal append's scope must work even with the same signal pending.
        assert!(
            output(&mut command(root, &["--version"]), None)
                .unwrap()
                .contains("git version")
        );
        assert_eq!(watch.take(), Some(libc::SIGTERM));
        return;
    }
    let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    cmd.args(["--exact", TEST, "--nocapture"]).env(CHILD, "1");
    let out =
        aterm_uds::deadline::output_within(&mut cmd, std::time::Duration::from_secs(60)).unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn only_decided_verdicts_or_unexpired_claims_exclude_another_sample() {
    let mut r = Record {
        commit: "c".into(),
        tree: "t".into(),
        host: "h".into(),
        verifier: "v".into(),
        controller: String::new(),
        inputs: "i".into(),
        token: "one".into(),
        state: "judging".into(),
        cause: String::new(),
        started: 0,
        updated: 0,
        lease_until: 120,
        rows: vec![],
    };
    assert!(r.occupied(119));
    assert!(!r.occupied(120));
    for state in ["cancelled", "could-not-run"] {
        r.state = state.into();
        assert!(!r.occupied(1));
    }
    for state in ["pass", "red"] {
        r.state = state.into();
        assert!(r.occupied(1000));
    }
}

#[test]
fn competing_append_cannot_replace_a_claim_and_pruning_keeps_recent_rows() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let root = tmp.path();
    git(root, &["init", "--bare", "remote"]).unwrap();
    git(root, &["clone", "remote", "one"]).unwrap();
    let one = root.join("one");
    git(&one, &["config", "user.name", "Andrew Yates"]).unwrap();
    git(&one, &["config", "user.email", "test@example.invalid"]).unwrap();
    let mut r = Record {
        commit: "c".into(),
        tree: "t".into(),
        host: "h".into(),
        verifier: "v".into(),
        controller: String::new(),
        inputs: "i".into(),
        token: "one".into(),
        state: "judging".into(),
        cause: String::new(),
        started: 0,
        updated: 1,
        lease_until: 120,
        rows: vec![],
    };
    assert!(append(&one, "origin", None, &r, deadline()).unwrap());
    let first = head(&one, "origin", deadline()).unwrap().unwrap();
    assert!(
        append(
            &one,
            "origin",
            Some(&first),
            &r,
            std::time::Instant::now() - std::time::Duration::from_secs(1)
        )
        .is_err()
    );
    assert_eq!(
        head(&one, "origin", deadline()).unwrap().as_deref(),
        Some(first.as_str())
    );
    let blob = git(&one, &["rev-parse", &format!("{first}:verdicts/t.toml")]).unwrap();
    let input = format!("{blob}\n").repeat(3000);
    let objects = output(
        &mut command(&one, &["cat-file", "--batch"]),
        Some(input.as_bytes()),
    )
    .unwrap();
    assert_eq!(
        decode_objects(objects.as_bytes()).unwrap().len(),
        3000,
        "large journals must drain output while feeding more object requests"
    );
    r.token = "two".into();
    assert!(!append(&one, "origin", None, &r, deadline()).unwrap());
    assert_eq!(
        records(&one, Some(&first), deadline()).unwrap()[0].token,
        "one"
    );
    r.tree = "new".into();
    r.updated = RETENTION + 2;
    assert!(append(&one, "origin", Some(&first), &r, deadline()).unwrap());
    let last = head(&one, "origin", deadline()).unwrap().unwrap();
    assert_eq!(records(&one, Some(&last), deadline()).unwrap(), vec![r]);
}

/// A lane record of `tree`'s attempt `token`, `state` at `updated`.
fn lane_record(tree: &str, token: &str, state: &str, updated: u64) -> Record {
    Record {
        commit: "c".into(),
        tree: tree.into(),
        host: "h".into(),
        verifier: "v".into(),
        controller: String::new(),
        inputs: "i".into(),
        token: token.into(),
        state: state.into(),
        cause: String::new(),
        started: updated,
        updated,
        lease_until: updated + LEASE,
        rows: vec![],
    }
}

/// A scratch repository with a committer, and an empty bare remote for each of `remotes`.
fn scratch(remotes: &[&str]) -> (aterm_tempfile::TempDir, std::path::PathBuf) {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let root = tmp.path().join("one");
    git(tmp.path(), &["init", "--quiet", "one"]).unwrap();
    git(&root, &["config", "user.name", "Andrew Yates"]).unwrap();
    git(&root, &["config", "user.email", "test@example.invalid"]).unwrap();
    for remote in remotes {
        git(tmp.path(), &["init", "--bare", "--quiet", remote]).unwrap();
        let url = tmp.path().join(remote);
        git(&root, &["remote", "add", remote, url.to_str().unwrap()]).unwrap();
    }
    (tmp, root)
}

fn rev(root: &Path, spec: &str) -> String {
    git(root, &["rev-parse", spec]).unwrap()
}

fn blob_of(root: &Path, text: &str) -> String {
    output(
        &mut command(root, &["hash-object", "-w", "--stdin"]),
        Some(text.as_bytes()),
    )
    .unwrap()
    .trim()
    .to_owned()
}

fn push(root: &Path, remote: &str, commit: &str) {
    git(
        root,
        &["push", "--porcelain", remote, &format!("{commit}:{REF}")],
    )
    .unwrap();
}

/// `base` with `edits` applied the way `append` once built every tree: the whole journal read
/// into a fresh index, each edit an update of it, the tree written from it. The oracle the
/// path edit is held to.
fn by_index(root: &Path, base: Option<&str>, edits: &[Edit]) -> String {
    let dir = aterm_tempfile::tempdir().unwrap();
    let index = dir.path().join("index");
    let run = |args: &[&str], input: Option<&[u8]>| {
        let mut cmd = command(root, args);
        cmd.env("GIT_INDEX_FILE", &index);
        output(&mut cmd, input).unwrap()
    };
    run(&["read-tree", base.unwrap_or("--empty")], None);
    for edit in edits {
        match edit {
            Edit::Put { path, blob } => run(
                &["update-index", "--add", "--cacheinfo", "100644", blob, path],
                None,
            ),
            Edit::Remove { path } => run(
                &[
                    "rm",
                    "-r",
                    "-q",
                    "-f",
                    "--cached",
                    "--ignore-unmatch",
                    "--",
                    path,
                ],
                None,
            ),
        };
    }
    run(&["write-tree"], None).trim().to_owned()
}

/// A commit over `parent` whose tree adds `paths`, each the blob of `body`, built at once as a
/// fixture: one index, one update of it.
fn with_files(root: &Path, parent: &str, paths: &[String], body: &str) -> String {
    let blob = blob_of(root, body);
    let dir = aterm_tempfile::tempdir().unwrap();
    let index = dir.path().join("index");
    let run = |args: &[&str], input: Option<&[u8]>| {
        let mut cmd = command(root, args);
        cmd.env("GIT_INDEX_FILE", &index);
        output(&mut cmd, input).unwrap()
    };
    run(&["read-tree", parent], None);
    let lines: String = paths
        .iter()
        .map(|path| format!("100644 blob {blob}\t{path}\n"))
        .collect();
    run(&["update-index", "--index-info"], Some(lines.as_bytes()));
    let tree = run(&["write-tree"], None);
    git(
        root,
        &["commit-tree", tree.trim(), "-p", parent, "-m", "fixture"],
    )
    .unwrap()
}

/// The ids of the tree objects one edit lists and makes, in order.
#[derive(Default)]
struct Counted {
    listed: Vec<String>,
    made: Vec<String>,
}

impl Counted {
    /// Run `edit` through trees that count what it reads and writes in `root`.
    fn of<T>(root: &Path, edit: impl FnOnce(&mut Trees<'_>) -> T) -> (T, Self) {
        let until = deadline();
        let mut counted = Self::default();
        let Self { listed, made } = &mut counted;
        let done = edit(&mut Trees {
            list: &mut |id| {
                listed.push(id.to_owned());
                ls_tree(root, id, until)
            },
            make: &mut |entries| {
                let id = mktree(root, entries, until)?;
                made.push(id.clone());
                Ok(id)
            },
        });
        (done, counted)
    }
}

/// An append rewrites only the trees on the paths it edits. A heartbeat over a journal with
/// no `land/` entries and over one with 10,000 reads and writes the same trees, the root and
/// `verdicts/`, and keeps `land/` by its id; a record under `land/<day>/<T>/` writes the root,
/// `land/`, `land/<day>/` and `<T>/`, whether `<T>/` is new or there; a removal drops what it
/// empties. CONTROL: every tree is the one the old append's whole-journal `read-tree` /
/// `write-tree` makes (`by_index`), every land entry kept. NEGATIVE CONTROL: list and make each
/// untouched subtree again, and the heartbeat over 10,000 entries reads and writes `land/` too.
#[test]
fn an_append_writes_only_the_trees_it_touches() {
    let (_tmp, one) = scratch(&["origin"]);
    let at = 20_404 * 86_400 + 100;
    let mut r = lane_record("t0", "one", "judging", at);
    assert!(append(&one, "origin", None, &r, deadline()).unwrap());
    let small = head(&one, "origin", deadline()).unwrap().unwrap();
    let paths: Vec<String> = (0..10_000u32)
        .map(|i| format!("land/{}/{i:040x}/lint.{i:064x}.ok.toml", 20_400 + i % 5))
        .collect();
    let mut heartbeat = |parent: &str, updated: u64| {
        r.updated = updated;
        let (pushed, counted) = Counted::of(&one, |trees| {
            append_with(&one, "origin", Some(parent), &r, deadline(), trees).unwrap()
        });
        assert!(pushed);
        let after = head(&one, "origin", deadline()).unwrap().unwrap();
        let record = blob_of(&one, &aterm_toml::to_string(&r).unwrap());
        (after, counted, record)
    };
    let (after_small, counted_small, _) = heartbeat(&small, at + 1);
    let large = with_files(&one, &after_small, &paths, "a land record");
    push(&one, "origin", &large);
    let (after_large, counted_large, record) = heartbeat(&large, at + 2);
    for (parent, after, counted) in [
        (&small, &after_small, &counted_small),
        (&large, &after_large, &counted_large),
    ] {
        assert_eq!(
            counted.listed,
            [parent.clone(), rev(&one, &format!("{parent}:verdicts"))],
            "a heartbeat reads the root and verdicts/"
        );
        assert_eq!(
            counted.made,
            [
                rev(&one, &format!("{after}:verdicts")),
                rev(&one, &format!("{after}^{{tree}}"))
            ],
            "and writes only them"
        );
    }
    assert_eq!(
        rev(&one, &format!("{after_large}:land")),
        rev(&one, &format!("{large}:land")),
        "land/ is kept by its id"
    );
    let put = |path: &str, blob: &str| Edit::Put {
        path: path.into(),
        blob: blob.into(),
    };
    assert_eq!(
        rev(&one, &format!("{after_large}^{{tree}}")),
        by_index(&one, Some(&large), &[put("verdicts/t0.toml", &record)]),
        "the heartbeat's tree is the whole-journal append's"
    );
    assert_eq!(
        git(
            &one,
            &["ls-tree", "-r", "--name-only", &after_large, "land/"]
        )
        .unwrap()
        .lines()
        .count(),
        10_000
    );

    let body = blob_of(&one, "a new land record");
    for (path, listed) in [
        (
            format!("land/20404/{:040x}/lint.{:064x}.FAIL.toml", 10_000, 1),
            3,
        ),
        (format!("land/20404/{:040x}/test.{:064x}.ok.toml", 4, 4), 4),
    ] {
        let edits = [put(&path, &body)];
        let (tree, counted) = Counted::of(&one, |trees| {
            edit_tree_with(Some(&after_large), &edits, trees).unwrap()
        });
        let dir = path.rsplit_once('/').unwrap().0;
        assert_eq!(
            counted.made,
            [
                rev(&one, &format!("{tree}:{dir}")),
                rev(&one, &format!("{tree}:land/20404")),
                rev(&one, &format!("{tree}:land")),
                tree.clone()
            ],
            "{path}"
        );
        assert_eq!(counted.listed.len(), listed, "{path}: {:?}", counted.listed);
        assert_eq!(tree, by_index(&one, Some(&after_large), &edits), "{path}");
    }

    let edits = [
        Edit::Remove {
            path: format!("land/20404/{:040x}/lint.{:064x}.ok.toml", 4, 4),
        },
        Edit::Remove {
            path: "land/20403".into(),
        },
        Edit::Remove {
            path: "land/20402/absent.toml".into(),
        },
    ];
    let (tree, counted) = Counted::of(&one, |trees| {
        edit_tree_with(Some(&after_large), &edits, trees).unwrap()
    });
    assert_eq!(tree, by_index(&one, Some(&after_large), &edits));
    assert_eq!(
        counted.made,
        [
            rev(&one, &format!("{tree}:land/20404")),
            rev(&one, &format!("{tree}:land")),
            tree.clone()
        ],
        "the emptied <T>/ is dropped, and an absent path changes nothing"
    );
    assert_eq!(
        git(&one, &["ls-tree", "--name-only", &tree, "land/"]).unwrap(),
        "land/20400\nland/20401\nland/20402\nland/20404"
    );
}

/// The lane's readers read `verdicts/` alone. Over a journal whose chain also holds `land/`
/// entries, each shaped as a lane record (one under a nested `verdicts/`), `records`, `record`
/// and `history` return exactly what they return over the same appends without them, the
/// control. NEGATIVE CONTROL: let `records` or `history` read `land/` beside `verdicts/`, and
/// the land entries are read as the lane's.
#[test]
fn lane_readers_ignore_land() {
    let (_tmp, one) = scratch(&["plain", "landed"]);
    let at = 20_404 * 86_400;
    let steps = [
        ("t1", "first", "judging", 0),
        ("t1", "first", "cancelled", 1),
        ("t2", "second", "judging", 2),
        ("t2", "second", "pass", 3),
    ];
    for (step, (tree, token, state, delta)) in steps.into_iter().enumerate() {
        let r = lane_record(tree, token, state, at + delta);
        for remote in ["plain", "landed"] {
            let parent = head(&one, remote, deadline()).unwrap();
            assert!(append(&one, remote, parent.as_deref(), &r, deadline()).unwrap());
        }
        if step == 1 {
            let parent = head(&one, "landed", deadline()).unwrap().unwrap();
            let body = aterm_toml::to_string(&lane_record("t9", "land", "pass", at + 1)).unwrap();
            let land = with_files(
                &one,
                &parent,
                &[
                    format!("land/20404/{:040x}/lint.{:064x}.ok.toml", 9, 9),
                    format!("land/20404/{:040x}/verdicts/t9.toml", 9),
                ],
                &body,
            );
            push(&one, "landed", &land);
        }
    }
    let plain = head(&one, "plain", deadline()).unwrap().unwrap();
    let landed = head(&one, "landed", deadline()).unwrap().unwrap();
    assert_eq!(
        git(&one, &["ls-tree", "-r", "--name-only", &landed, "land/"])
            .unwrap()
            .lines()
            .count(),
        2,
        "the land entries are on the landed journal's head"
    );
    let lane = records(&one, Some(&plain), deadline()).unwrap();
    assert_eq!(lane.len(), 2);
    assert_eq!(records(&one, Some(&landed), deadline()).unwrap(), lane);
    for tree in ["t1", "t2", "t9"] {
        assert_eq!(
            record(&one, Some(&landed), tree, deadline()).unwrap(),
            record(&one, Some(&plain), tree, deadline()).unwrap(),
            "{tree}"
        );
    }
    for since in [0, at + 2] {
        let attempts = history(&one, &plain, since).unwrap();
        assert!(!attempts.is_empty());
        assert_eq!(history(&one, &landed, since).unwrap(), attempts, "{since}");
    }
}

/// A prune removes each stale record by the path its own tree names, and a tree no journal
/// entry can be filed under removes nothing, as the old append's `update-index --force-remove`
/// removed nothing there: a path with an empty or `..` component, one below a file, one below
/// a path the same prune removes. The append still lands and `pruned-at` advances, so the
/// next append does not meet the same record again. CONTROL: the tree is the one the old
/// append's whole-journal index makes of the same edits (`by_index`). NEGATIVE CONTROL: refuse
/// such a removal, as a put of its path is refused, and every append over this journal fails.
#[test]
fn a_prune_removes_nothing_where_a_records_tree_names_no_journal_entry() {
    let (_tmp, one) = scratch(&["origin"]);
    let day = 20_404 * 86_400;
    let old = day - RETENTION - 86_400;
    let first = lane_record("z", "z", "pass", old);
    assert!(append(&one, "origin", None, &first, deadline()).unwrap());
    let mut parent = head(&one, "origin", deadline()).unwrap().unwrap();
    for (path, tree, updated) in [
        ("verdicts/x.toml", "x", day),
        ("verdicts/empty.toml", "a//b", old),
        ("verdicts/up.toml", "../up", old),
        ("verdicts/below.toml", "x.toml/y", old),
        ("verdicts/zz.toml", "z.toml/w", old),
    ] {
        let body = aterm_toml::to_string(&lane_record(tree, tree, "pass", updated)).unwrap();
        parent = with_files(&one, &parent, &[path.to_owned()], &body);
    }
    push(&one, "origin", &parent);
    let r = lane_record("t", "t", "judging", day + 1);
    assert_eq!(
        append(&one, "origin", Some(&parent), &r, deadline()),
        Ok(true)
    );
    let after = head(&one, "origin", deadline()).unwrap().unwrap();
    assert_eq!(
        git(&one, &["show", &format!("{after}:pruned-at")]).unwrap(),
        (day / 86_400).to_string(),
        "the prune is recorded, so the next append does not run it again"
    );
    let remove = |path: &str| Edit::Remove { path: path.into() };
    let edits = [
        remove("verdicts/x.toml/y.toml"),
        remove("verdicts/a//b.toml"),
        remove("verdicts/../up.toml"),
        remove("verdicts/z.toml"),
        remove("verdicts/z.toml/w.toml"),
        Edit::Put {
            path: "pruned-at".into(),
            blob: blob_of(&one, &(day / 86_400).to_string()),
        },
        Edit::Put {
            path: "verdicts/t.toml".into(),
            blob: blob_of(&one, &aterm_toml::to_string(&r).unwrap()),
        },
    ];
    assert_eq!(
        rev(&one, &format!("{after}^{{tree}}")),
        by_index(&one, Some(&parent), &edits),
        "the tree is the old append's"
    );
    assert_eq!(
        git(&one, &["ls-tree", "--name-only", &after, "verdicts/"]).unwrap(),
        "verdicts/below.toml\nverdicts/empty.toml\nverdicts/t.toml\nverdicts/up.toml\n\
         verdicts/x.toml\nverdicts/zz.toml",
        "only the stale record its own tree names is removed"
    );
}
