// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A fixture's git child: it must finish, so it runs while it makes progress under the
/// one-minute hang detector (AGENTS.md), never under the production journal's 60 s wall clock
/// (`judged::deadline`). Building a fixture journal writes one loose object per tree, and on a
/// loaded machine that file creation alone outlasts a minute while git keeps working. The
/// journal's own operations under test keep their production deadline.
fn fixture(cmd: &mut Command, input: Option<&[u8]>) -> Result<String, String> {
    let dir = aterm_tempfile::tempdir().map_err(|e| e.to_string())?;
    let stdin = match input {
        Some(bytes) => {
            let path = dir.path().join("stdin");
            std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
            std::fs::File::open(&path)
                .map_err(|e| e.to_string())?
                .into()
        }
        None => aterm_uds::spawnfd::Stdio::null(),
    };
    let out = aterm_uds::deadline::output_from_while_process_progressing_complete_with_limit(
        cmd,
        stdin,
        aterm_uds::deadline::ECHO_HANG,
        "a judged fixture's git",
        aterm_uds::deadline::OUTPUT_KEEP - 1,
    )
    .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }
    String::from_utf8(out.stdout).map_err(|e| e.to_string())
}

/// Fixture git, shadowing the journal's own `git` for every set-up and inspection call here.
fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    fixture(&mut command(root, args), None).map(|s| s.trim().to_owned())
}

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

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn raw_landing_interrupts_cancel_scoped_renewals_but_not_ordinary_journal_publication() {
    const CHILD: &str = "ATERM_VERIFY_RAW_JOURNAL_INTERRUPT_CHILD";
    const TEST: &str = "judged::tests::raw_landing_interrupts_cancel_scoped_renewals_but_not_ordinary_journal_publication";
    if std::env::var_os(CHILD).is_some() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "--quiet"]).unwrap();
        git(root, &["config", "user.name", "Andrew Yates"]).unwrap();
        git(root, &["config", "user.email", "test@example.invalid"]).unwrap();
        git(root, &["init", "--bare", "--quiet", "remote"]).unwrap();
        git(root, &["remote", "add", "origin", "remote"]).unwrap();
        let record = lane_record("terminal", "attempt", "pass", 42);
        crate::exec::group::kill_on_interrupt();
        let _held = crate::exec::group::hold();
        // SAFETY: this isolated child installed the real landing handler, and the live hold
        // prevents its watchdog from ending the process before publication completes.
        #[expect(
            clippy::disallowed_methods,
            reason = "the isolated journal fixture must catch a real non-core SIGTERM to verify publication policy; it must not die by SIGKILL"
        )]
        let raised = unsafe { libc::raise(libc::SIGTERM) };
        assert_eq!(raised, 0);
        assert_eq!(crate::exec::group::taken(), Some(libc::SIGTERM));
        let scoped = interruptible(|| append(root, "origin", None, &record, deadline()));
        assert!(scoped.unwrap_err().contains("interrupted"));
        assert!(head(root, "origin", deadline()).unwrap().is_none());
        assert!(append(root, "origin", None, &record, deadline()).unwrap());
        let published = head(root, "origin", deadline()).unwrap().unwrap();
        assert_eq!(
            records(root, Some(&published), deadline()).unwrap(),
            [record]
        );
        assert_eq!(crate::exec::group::taken(), Some(libc::SIGTERM));
        drop(dir);
        return;
    }
    let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    command
        .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
        .env(CHILD, "1");
    let output = aterm_uds::deadline::output_while_process_progressing(
        &mut command,
        aterm_uds::deadline::ECHO_HANG,
        "isolated raw-interrupt journal publication",
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
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
        decode_objects(objects.as_bytes(), &vec![blob.as_str(); 3000])
            .unwrap()
            .len(),
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
    // These disposable repositories have no owner for detached object maintenance.
    git(&root, &["config", "maintenance.auto", "false"]).unwrap();
    git(&root, &["config", "user.name", "Andrew Yates"]).unwrap();
    git(&root, &["config", "user.email", "test@example.invalid"]).unwrap();
    for remote in remotes {
        git(tmp.path(), &["init", "--bare", "--quiet", remote]).unwrap();
        let url = tmp.path().join(remote);
        git(&url, &["config", "maintenance.auto", "false"]).unwrap();
        git(&root, &["remote", "add", remote, url.to_str().unwrap()]).unwrap();
    }
    (tmp, root)
}

fn rev(root: &Path, spec: &str) -> String {
    git(root, &["rev-parse", spec]).unwrap()
}

fn blob_of(root: &Path, text: &str) -> String {
    fixture(
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
        fixture(&mut cmd, input).unwrap()
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
        fixture(&mut cmd, input).unwrap()
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
/// no `land/` entries and over two entries on each of five days reads and writes only the root
/// and `verdicts/`, keeping `land/` by its id; a record under `land/<day>/<T>/` writes the root,
/// `land/`, `land/<day>/` and `<T>/`, whether `<T>/` is new or there; a removal drops what it
/// empties. CONTROL: every tree is the one the old append's whole-journal `read-tree` /
/// `write-tree` makes (`by_index`), every land entry kept. NEGATIVE CONTROL: list and make each
/// untouched subtree again, and the populated journal's heartbeat reads and writes `land/` too.
#[test]
fn an_append_writes_only_the_trees_it_touches() {
    let (_tmp, one) = scratch(&["origin"]);
    let at = 20_404 * 86_400 + 100;
    let mut r = lane_record("t0", "one", "judging", at);
    assert!(append(&one, "origin", None, &r, deadline()).unwrap());
    let small = head(&one, "origin", deadline()).unwrap().unwrap();
    let paths: Vec<String> = (0..10u32)
        .map(|i| format!("land/{}/{i:040x}/lint.{i:064x}.ok.toml", 20_400 + i % 5))
        .collect();
    let mut heartbeat = |parent: &str, updated: u64| {
        r.updated = updated;
        let (pushed, counted) = Counted::of(&one, |trees| {
            append_with(
                &one,
                "origin",
                Some(parent),
                &r,
                Place::Slot,
                deadline(),
                trees,
            )
            .unwrap()
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
            &["ls-tree", "-r", "--object-only", &after_large, "land/"]
        )
        .unwrap()
        .lines()
        .count(),
        paths.len()
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

/// A commit over `parent` whose tree puts each blob of `files` at its path, built at once as a
/// fixture.
fn with_blobs(root: &Path, parent: &str, files: &[(String, Vec<u8>)]) -> String {
    let dir = aterm_tempfile::tempdir().unwrap();
    let index = dir.path().join("index");
    let run = |args: &[&str], input: Option<&[u8]>| {
        let mut cmd = command(root, args);
        cmd.env("GIT_INDEX_FILE", &index);
        fixture(&mut cmd, input).unwrap()
    };
    run(&["read-tree", parent], None);
    let lines: String = files
        .iter()
        .map(|(path, bytes)| {
            let blob = run(&["hash-object", "-w", "--stdin"], Some(bytes));
            format!("100644 blob {}\t{path}\n", blob.trim())
        })
        .collect();
    run(&["update-index", "--index-info"], Some(lines.as_bytes()));
    let tree = run(&["write-tree"], None);
    git(
        root,
        &["commit-tree", tree.trim(), "-p", parent, "-m", "fixture"],
    )
    .unwrap()
}

/// The bytes at `path` on `head`.
fn bytes_at(root: &Path, head: &str, path: &str) -> Vec<u8> {
    capture_until(
        &mut command(root, &["cat-file", "blob", &format!("{head}:{path}")]),
        None,
        deadline(),
    )
    .unwrap()
    .stdout
}

/// `bytes` with the first digit after `marker` changed.
fn flipped(bytes: &[u8], marker: &str) -> Vec<u8> {
    let text = std::str::from_utf8(bytes).unwrap();
    let at = text.find(marker).unwrap() + marker.len();
    let mut out = bytes.to_vec();
    out[at] = if out[at] == b'1' { b'2' } else { b'1' };
    out
}

/// Exit test "the digest matches on read-back": signed records appended through one clone read
/// back in a second clone of the same remote with the same blob ids and the same bytes, parse to
/// the records written, and verify under the writer's key. CONTROL: the same records with one
/// byte of each body changed, at the same paths, are unreadable: their signatures do not verify,
/// while their names still say the pass and the FAIL. NEGATIVE CONTROL, run as a mutant that made
/// this test fail: skip the signature check in `land_record::verify` (the changed records read).
#[test]
fn a_land_record_reads_back_byte_for_byte_and_its_signature_verifies() {
    use crate::checks::Check;
    use crate::land_record::Word;
    use crate::land_record::tests::{BASE, NOW, T, record, signer};
    let (tmp, one) = scratch(&["origin"]);
    let two = tmp.path().join("two");
    git(tmp.path(), &["init", "--quiet", "two"]).unwrap();
    let url = tmp.path().join("origin");
    git(&two, &["remote", "add", "origin", url.to_str().unwrap()]).unwrap();
    let me = signer(7);
    let written = [
        record(Check::Lint, Word::Ok, T, BASE, NOW),
        record(Check::Test, Word::Fail, T, BASE, NOW),
    ];
    let signed: Vec<_> = written.iter().map(|r| r.sign(Some(&me)).unwrap()).collect();
    let pushed = append_land(&one, "origin", &signed, NOW, deadline())
        .unwrap()
        .unwrap();
    let fetched = head(&two, "origin", deadline()).unwrap();
    assert_eq!(fetched.as_deref(), Some(pushed.as_str()));
    let entries = land_records(&two, fetched.as_deref(), &[T], deadline()).unwrap();
    assert_eq!(entries.len(), 2, "{entries:?}");
    for (record, signed) in written.iter().zip(&signed) {
        let path = &signed.path;
        assert_eq!(
            rev(&two, &format!("{pushed}:{path}")),
            rev(&one, &format!("{pushed}:{path}")),
            "{path}"
        );
        assert_eq!(bytes_at(&two, &pushed, path), signed.bytes, "{path}");
        let entry = entries.iter().find(|e| e.path == *path).unwrap();
        let read = entry.body.as_ref().unwrap();
        assert_eq!(&read.record, record, "{path}");
        assert_eq!(
            read.signature.as_ref().map(|s| (s.node.as_str(), s.key)),
            Some((me.node(), me.key())),
            "{path}"
        );
    }
    let changed: Vec<(String, Vec<u8>)> = signed
        .iter()
        .map(|s| (s.path.clone(), flipped(&s.bytes, "started = ")))
        .collect();
    let corrupt = with_blobs(&one, &pushed, &changed);
    push(&one, "origin", &corrupt);
    let fetched = head(&two, "origin", deadline()).unwrap();
    let entries = land_records(&two, fetched.as_deref(), &[T], deadline()).unwrap();
    assert_eq!(entries.len(), 2, "{entries:?}");
    for entry in &entries {
        let why = entry.body.as_ref().unwrap_err();
        assert!(why.contains("signature does not verify"), "{why}");
    }
    let mut words: Vec<_> = entries
        .iter()
        .map(|entry| entry.filed.as_ref().unwrap().name.word.clone())
        .collect();
    words.sort();
    assert_eq!(words, [Ok(Word::Ok), Ok(Word::Fail)]);
}

/// A selected tree bucket replaced by a file is visible as unreadable. Selecting its
/// neighbor still reads that neighbor's valid record without widening to the malformed path.
#[test]
fn verdict_records_report_a_file_replacing_the_selected_tree_bucket() {
    use crate::checks::Check;
    use crate::land_record::Word;
    use crate::land_record::tests::{BASE, DAY, NOW, T, T2, record, signer};
    let (_tmp, root) = scratch(&[]);
    git(
        &root,
        &["commit", "--quiet", "--allow-empty", "-m", "empty"],
    )
    .unwrap();
    let empty = rev(&root, "HEAD");
    let neighbor = record(Check::Fmt, Word::Ok, T2, BASE, NOW)
        .sign(Some(&signer(7)))
        .unwrap();
    let path = format!("{LAND}/{DAY}/{T}");
    let tree = with_blobs(
        &root,
        &empty,
        &[
            (path.clone(), b"not a directory".to_vec()),
            (neighbor.path.clone(), neighbor.bytes),
        ],
    );
    let entries = land_records(&root, Some(&tree), &[T], deadline()).unwrap();
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0].path, path);
    assert!(entries[0].body.is_err());
    let entries = land_records(&root, Some(&tree), &[T2], deadline()).unwrap();
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0].path, neighbor.path);
    assert!(entries[0].body.is_ok(), "{entries:?}");
}

/// Git stores symlink targets as blobs too. Valid signed bytes do not make that tree
/// entry a regular record file; an executable regular file remains readable.
#[test]
fn journal_records_require_regular_files_even_when_a_link_holds_valid_signed_bytes() {
    use crate::checks::Check;
    use crate::land_record::Word;
    use crate::land_record::tests::{BASE, NOW, T, record, signer};
    let (_tmp, root) = scratch(&["origin"]);
    let signed = record(Check::Lint, Word::Ok, T, BASE, NOW)
        .sign(Some(&signer(7)))
        .unwrap();
    let parent = append_land(
        &root,
        "origin",
        std::slice::from_ref(&signed),
        NOW,
        deadline(),
    )
    .unwrap()
    .unwrap();
    let blob = rev(&root, &format!("{parent}:{}", signed.path));
    for (mode, readable) in [("100755", true), ("120000", false)] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let index = dir.path().join("index");
        let run = |args: &[&str]| {
            let mut cmd = command(&root, args);
            cmd.env("GIT_INDEX_FILE", &index);
            fixture(&mut cmd, None).unwrap()
        };
        run(&["read-tree", &parent]);
        run(&["update-index", "--cacheinfo", mode, &blob, &signed.path]);
        let tree = run(&["write-tree"]);
        assert_eq!(bytes_at(&root, tree.trim(), &signed.path), signed.bytes);
        let entries = land_records(&root, Some(tree.trim()), &[T], deadline()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].body.is_ok(),
            readable,
            "mode={mode}: {entries:?}"
        );
        if !readable {
            assert!(
                entries[0]
                    .body
                    .as_ref()
                    .unwrap_err()
                    .contains("regular file")
            );
        }
    }
}

/// A FAIL's path keeps the first record filed there: a second FAIL record at the same path,
/// from a writer whose append raced the first and so re-read the journal under the CAS, leaves
/// the first in place and pushes nothing; so does a second FAIL later in one batch. CONTROL: an
/// `ok` over an older `ok` at its path is replaced. NEGATIVE CONTROL, run as a mutant that made
/// this test fail: let `land_append_with` put a FAIL already on its parent (the second
/// replaces the first).
#[test]
fn a_fail_record_is_never_replaced() {
    use crate::checks::Check;
    use crate::land_record::Word;
    use crate::land_record::tests::{BASE, NOW, T, record, signer};
    let (_tmp, one) = scratch(&["origin"]);
    let me = signer(7);
    let fail = |id: &str, word| {
        let mut r = record(Check::Lint, word, T, BASE, NOW);
        r.run.id = id.into();
        r.sign(Some(&me)).unwrap()
    };
    let (first, second) = (fail("first", Word::Fail), fail("second", Word::Fail));
    assert_eq!(first.path, second.path);
    assert_ne!(first.bytes, second.bytes);
    let stale = head(&one, "origin", deadline()).unwrap();
    let held = append_land(
        &one,
        "origin",
        std::slice::from_ref(&first),
        NOW,
        deadline(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        land_append(
            &one,
            "origin",
            stale.as_deref(),
            std::slice::from_ref(&second),
            NOW,
            deadline()
        ),
        Ok(Appended::Raced(Some(held.clone()))),
        "the second writer raced the first"
    );
    assert_eq!(
        append_land(
            &one,
            "origin",
            std::slice::from_ref(&second),
            NOW,
            deadline()
        ),
        Ok(Some(held.clone())),
        "re-read under the CAS, it has nothing to write"
    );
    assert_eq!(bytes_at(&one, &held, &first.path), first.bytes);

    let mut later = record(Check::Test, Word::Fail, T, BASE, NOW);
    later.run.id = "one".into();
    let batch_first = later.sign(Some(&me)).unwrap();
    later.run.id = "two".into();
    let batch_second = later.sign(Some(&me)).unwrap();
    let after = append_land(
        &one,
        "origin",
        &[batch_first.clone(), batch_second],
        NOW,
        deadline(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(bytes_at(&one, &after, &batch_first.path), batch_first.bytes);

    let (older, newer) = (fail("older", Word::Ok), fail("newer", Word::Ok));
    append_land(&one, "origin", &[older], NOW, deadline()).unwrap();
    let last = append_land(
        &one,
        "origin",
        std::slice::from_ref(&newer),
        NOW,
        deadline(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        bytes_at(&one, &last, &newer.path),
        newer.bytes,
        "CONTROL: a pass takes the newer record"
    );
}

/// The land prune keeps its own day marker, `land/pruned-at`: after a lane append has written
/// the day's `pruned-at` first, the day's first land append still drops each `land/<day>/` past
/// `LAND_RETENTION` and keeps a younger one, writes `land/pruned-at`, leaves the lane's
/// `pruned-at` alone, and appends no record filed under a day it drops. CONTROL: a second land
/// append the same day prunes nothing. NEGATIVE CONTROL, run as a mutant that made this test
/// fail: read the lane's `pruned-at` for the land prune (the expired day stays).
#[test]
fn the_land_prune_runs_on_its_own_marker() {
    use crate::checks::Check;
    use crate::land_record::Word;
    use crate::land_record::tests::{BASE, DAY, NOW, T, T2, record, signer};
    let (_tmp, one) = scratch(&["origin"]);
    let me = signer(7);
    assert!(
        append(
            &one,
            "origin",
            None,
            &lane_record("t0", "one", "judging", NOW),
            deadline()
        )
        .unwrap()
    );
    let lane = head(&one, "origin", deadline()).unwrap().unwrap();
    assert_eq!(
        git(&one, &["show", &format!("{lane}:pruned-at")]).unwrap(),
        DAY.to_string()
    );
    let at = |day: u64| day * 86_400 + 600;
    let old = record(Check::Lint, Word::Ok, T2, BASE, at(DAY - 3))
        .sign(Some(&me))
        .unwrap();
    let young = record(Check::Lint, Word::Ok, T2, BASE, at(DAY - 2))
        .sign(Some(&me))
        .unwrap();
    let seeded = with_blobs(
        &one,
        &lane,
        &[
            (old.path.clone(), old.bytes.clone()),
            (young.path.clone(), young.bytes.clone()),
        ],
    );
    push(&one, "origin", &seeded);
    let days = |head: &str| git(&one, &["ls-tree", "--name-only", head, "land/"]).unwrap();
    let expired = record(Check::Fmt, Word::Ok, T, BASE, at(DAY - 3))
        .sign(Some(&me))
        .unwrap();
    let fresh = record(Check::Fmt, Word::Ok, T, BASE, NOW)
        .sign(Some(&me))
        .unwrap();
    let pruned = append_land(
        &one,
        "origin",
        &[expired.clone(), fresh.clone()],
        NOW,
        deadline(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        days(&pruned),
        format!("land/{}\nland/{DAY}\nland/pruned-at", DAY - 2)
    );
    assert_eq!(
        git(&one, &["show", &format!("{pruned}:{LAND_PRUNED_AT}")]).unwrap(),
        DAY.to_string()
    );
    assert_eq!(
        rev(&one, &format!("{pruned}:pruned-at")),
        rev(&one, &format!("{lane}:pruned-at"))
    );
    assert_eq!(bytes_at(&one, &pruned, &fresh.path), fresh.bytes);
    assert!(
        git(&one, &["ls-tree", &pruned, &expired.path])
            .unwrap()
            .is_empty()
    );

    let reseeded = with_blobs(&one, &pruned, &[(old.path.clone(), old.bytes.clone())]);
    push(&one, "origin", &reseeded);
    let again = record(Check::Census, Word::Ok, T, BASE, NOW)
        .sign(Some(&me))
        .unwrap();
    let kept = append_land(&one, "origin", &[again], NOW, deadline())
        .unwrap()
        .unwrap();
    assert_eq!(
        days(&kept),
        format!(
            "land/{}\nland/{}\nland/{DAY}\nland/pruned-at",
            DAY - 3,
            DAY - 2
        ),
        "CONTROL: the day's second land append prunes nothing"
    );
}

/// Missing journals are empty; a file replacing either run-history directory is corruption,
/// and must be visible to the caller instead of looking like a journal with no runs.
#[test]
fn run_records_report_non_directory_roots() {
    let (_tmp, root) = scratch(&[]);
    git(
        &root,
        &["commit", "--quiet", "--allow-empty", "-m", "empty"],
    )
    .unwrap();
    let empty = rev(&root, "HEAD");
    for head in [None, Some(empty.as_str())] {
        assert!(run_records(&root, head, 0, deadline()).unwrap().is_empty());
    }
    for path in [LAND, RUNS] {
        let broken = with_blobs(&root, &empty, &[(path.into(), b"not a directory".to_vec())]);
        let entries = run_records(&root, Some(&broken), 0, deadline()).unwrap();
        assert_eq!(entries.len(), 1, "{path}: {entries:?}");
        assert_eq!(entries[0].path, path);
        assert_eq!(entries[0].origin, crate::land_record::Origin::Journal);
        assert_eq!(entries[0].body.as_ref().unwrap_err(), "it is no directory");
    }
}

/// RUN RECORDS KEEP FOURTEEN DAYS BESIDE VERDICTS OF TWO: one store, a retention per kind. The
/// first land append past `land/pruned-at`'s day drops the verdict day three days old and keeps
/// the run days three and fourteen days old, while it drops the run day fifteen days old; a run
/// record filed under a day its retention has expired is not appended. The verdict reader skips
/// `land/runs` (it names no unreadable entry there) and the run reader reads each run record kept,
/// from the day it is given on. NEGATIVE CONTROL, run as a mutant that made this test fail: prune
/// `land/runs/<day>/` by a verdict's retention (the run days three and fourteen days old go).
#[test]
fn run_records_keep_fourteen_days_beside_verdicts_of_two() {
    use crate::checks::Check;
    use crate::land_record::Word;
    use crate::land_record::tests::{BASE, DAY, NOW, T, record, signer};
    use crate::land_run::Kind;
    use crate::land_run::tests::record as run_record;
    let (_tmp, one) = scratch(&["origin"]);
    let me = signer(7);
    assert!(
        append(
            &one,
            "origin",
            None,
            &lane_record("t0", "one", "judging", NOW),
            deadline()
        )
        .unwrap()
    );
    let lane = head(&one, "origin", deadline()).unwrap().unwrap();
    let at = |day: u64| day * 86_400 + 600;
    let ran = |day: u64| {
        crate::land_run::Record {
            at: at(day),
            ..run_record(Kind::End, 0)
        }
        .sign(Some(&me))
        .unwrap()
    };
    let verdict_old = record(Check::Lint, Word::Ok, T, BASE, at(DAY - 3))
        .sign(Some(&me))
        .unwrap();
    let seeded: Vec<(String, Vec<u8>)> = [
        verdict_old.clone(),
        ran(DAY - 15),
        ran(DAY - 14),
        ran(DAY - 3),
    ]
    .into_iter()
    .map(|signed| (signed.path, signed.bytes))
    .collect();
    push(&one, "origin", &with_blobs(&one, &lane, &seeded));
    let fresh = record(Check::Fmt, Word::Ok, T, BASE, NOW)
        .sign(Some(&me))
        .unwrap();
    let pruned = append_land(
        &one,
        "origin",
        &[fresh.clone(), ran(DAY), ran(DAY - 16)],
        NOW,
        deadline(),
    )
    .unwrap()
    .unwrap();
    let ls = |dir: &str| git(&one, &["ls-tree", "--name-only", &pruned, dir]).unwrap();
    assert_eq!(
        ls("land/"),
        format!("land/{DAY}\nland/pruned-at\nland/runs")
    );
    assert_eq!(
        ls("land/runs/"),
        format!(
            "land/runs/{}\nland/runs/{}\nland/runs/{DAY}",
            DAY - 14,
            DAY - 3
        )
    );
    let verdicts = land_records(&one, Some(&pruned), &[T], deadline()).unwrap();
    assert_eq!(
        verdicts.iter().map(|e| e.path.as_str()).collect::<Vec<_>>(),
        [fresh.path.as_str()],
        "{verdicts:?}"
    );
    let runs = run_records(&one, Some(&pruned), 0, deadline()).unwrap();
    let days: Vec<u64> = runs
        .iter()
        .map(|entry| {
            let body = entry.body.as_ref().expect("it reads");
            body.record.at / 86_400
        })
        .collect();
    assert_eq!(days, [DAY - 14, DAY - 3, DAY]);
    assert_eq!(
        run_records(&one, Some(&pruned), DAY - 3, deadline())
            .unwrap()
            .len(),
        2,
        "the run reader reads from the day it is given on"
    );
}

/// The land prune goes by the earlier of the appending Mac's day and the lane's own (its
/// `pruned-at`): a Mac whose clock runs 3 days ahead drops no current record, the day's FAIL
/// included, and moves `land/pruned-at` no further than the lane's day, however often it
/// appends. CONTROL: the next day, after the lane's own append, a correct clock's land append
/// still drops the day past `LAND_RETENTION`. NEGATIVE CONTROL: with no lane day in the journal,
/// a land append prunes nothing and writes no `land/pruned-at`.
#[test]
fn a_clock_ahead_prunes_no_current_record() {
    use crate::checks::Check;
    use crate::land_record::Word;
    use crate::land_record::tests::{BASE, DAY, NOW, T, T2, record, signer};
    let me = signer(7);
    let at = |day: u64| day * 86_400 + 600;
    let signed = |check, word, tree: &str, ended| {
        record(check, word, tree, BASE, ended)
            .sign(Some(&me))
            .unwrap()
    };
    let old = signed(Check::Lint, Word::Ok, T2, at(DAY - 2));
    let current = signed(Check::Lint, Word::Fail, T, NOW);
    let days =
        |root: &Path, head: &str| git(root, &["ls-tree", "--name-only", head, "land/"]).unwrap();

    let (_tmp, one) = scratch(&["origin"]);
    assert!(
        append(
            &one,
            "origin",
            None,
            &lane_record("t0", "one", "judging", NOW),
            deadline()
        )
        .unwrap()
    );
    let lane = head(&one, "origin", deadline()).unwrap().unwrap();
    let seeded = with_blobs(
        &one,
        &lane,
        &[
            (old.path.clone(), old.bytes.clone()),
            (current.path.clone(), current.bytes.clone()),
        ],
    );
    push(&one, "origin", &seeded);
    let ahead = NOW + 3 * 86_400;
    let mut last = seeded;
    for _ in 0..2 {
        let record = signed(Check::Fmt, Word::Ok, T2, ahead);
        last = append_land(&one, "origin", &[record], ahead, deadline())
            .unwrap()
            .unwrap();
    }
    assert_eq!(
        days(&one, &last),
        format!(
            "land/{}\nland/{DAY}\nland/{}\nland/pruned-at",
            DAY - 2,
            DAY + 3
        )
    );
    assert_eq!(bytes_at(&one, &last, &current.path), current.bytes);
    assert_eq!(
        git(&one, &["show", &format!("{last}:{LAND_PRUNED_AT}")]).unwrap(),
        DAY.to_string(),
        "no further than the lane's day"
    );

    let tomorrow = NOW + 86_400;
    assert!(
        append(
            &one,
            "origin",
            Some(&last),
            &lane_record("t1", "two", "judging", tomorrow),
            deadline()
        )
        .unwrap()
    );
    let pruned = append_land(
        &one,
        "origin",
        &[signed(Check::Census, Word::Ok, T, tomorrow)],
        tomorrow,
        deadline(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        days(&one, &pruned),
        format!(
            "land/{DAY}\nland/{}\nland/{}\nland/pruned-at",
            DAY + 1,
            DAY + 3
        ),
        "CONTROL: the correct clock's next day drops the day past retention"
    );
    assert_eq!(
        git(&one, &["show", &format!("{pruned}:{LAND_PRUNED_AT}")]).unwrap(),
        (DAY + 1).to_string()
    );

    let (_tmp, two) = scratch(&["origin"]);
    let old_day = signed(Check::Lint, Word::Ok, T2, at(DAY - 3));
    let unpruned = append_land(&two, "origin", &[old_day], at(DAY - 3), deadline())
        .unwrap()
        .unwrap();
    let kept = append_land(
        &two,
        "origin",
        &[signed(Check::Fmt, Word::Ok, T, NOW)],
        NOW,
        deadline(),
    )
    .unwrap()
    .unwrap();
    assert_ne!(kept, unpruned);
    assert_eq!(
        days(&two, &kept),
        format!("land/{}\nland/{DAY}", DAY - 3),
        "NEGATIVE CONTROL: no lane day, no prune"
    );
}

/// A RECORD BESIDE ITS TREE'S SLOT IS THE JOURNAL'S AS ANY OTHER. `records` reads it with the
/// slot's and `record` reads the slot alone; `published` knows a record the journal has held, at
/// either place, at the head or before it; and a prune removes a stale record beside a slot by
/// its own path, leaving the slot's record to its own age. NEGATIVE CONTROL: prune it by the
/// slot its tree names, as every record was pruned before, and the fresh verdict goes while the
/// stale record stays.
#[test]
fn a_record_beside_a_slot_is_read_found_and_pruned_by_its_own_path() {
    let (_tmp, one) = scratch(&["origin"]);
    let day = 20_404 * 86_400;
    let old = day - RETENTION - 86_400;
    let fresh = lane_record("t", "held", "pass", day);
    assert!(append(&one, "origin", None, &fresh, deadline()).unwrap());
    let parent = head(&one, "origin", deadline()).unwrap();
    let late = lane_record("t", "late", "red", old);
    assert!(!published(&one, parent.as_deref(), &late, deadline()).unwrap());
    assert!(
        append_at(
            &one,
            "origin",
            parent.as_deref(),
            &late,
            Place::Beside,
            deadline()
        )
        .unwrap()
    );
    let parent = head(&one, "origin", deadline()).unwrap();
    assert_eq!(
        git(
            &one,
            &["ls-tree", "-r", "--name-only", parent.as_deref().unwrap()]
        )
        .unwrap(),
        "pruned-at\nverdicts/t.toml\nverdicts/t/late.toml"
    );
    let both = records(&one, parent.as_deref(), deadline()).unwrap();
    assert!(both.contains(&fresh) && both.contains(&late), "{both:?}");
    assert_eq!(
        record(&one, parent.as_deref(), "t", deadline()).unwrap(),
        Some(fresh.clone()),
        "a tree's verdict is its slot's"
    );
    let next = lane_record("u", "next", "judging", day + 86_400);
    assert!(append(&one, "origin", parent.as_deref(), &next, deadline()).unwrap());
    let after = head(&one, "origin", deadline()).unwrap();
    assert_eq!(
        git(
            &one,
            &[
                "ls-tree",
                "-r",
                "--name-only",
                after.as_deref().unwrap(),
                "verdicts/"
            ]
        )
        .unwrap(),
        "verdicts/t.toml\nverdicts/u.toml",
        "the stale record went by its own path, and the tree's fresh verdict stayed"
    );
    for held in [&fresh, &late, &next] {
        assert!(
            published(&one, after.as_deref(), held, deadline()).unwrap(),
            "the journal held {held:?}, pruned since or not"
        );
    }
    let never = lane_record("t", "never", "red", old);
    assert!(!published(&one, after.as_deref(), &never, deadline()).unwrap());
}

#[test]
fn publication_matches_literal_paths_under_inherited_pathspec_modes() {
    let (_tmp, one) = scratch(&["origin"]);
    let base = lane_record("base", "held", "pass", 42);
    assert!(append(&one, "origin", None, &base, deadline()).unwrap());
    let parent = head(&one, "origin", deadline()).unwrap().unwrap();
    let wanted = lane_record("t", "attempt*", "pass", 42);
    let bytes = aterm_toml::to_string(&wanted).unwrap().into_bytes();
    let elsewhere = with_blobs(
        &one,
        &parent,
        &[("verdicts/t/attempt-other.toml".into(), bytes.clone())],
    );
    assert!(
        !published(&one, Some(&elsewhere), &wanted, deadline()).unwrap(),
        "a wildcard must not turn another path into publication evidence"
    );
    let exact = with_blobs(&one, &elsewhere, &[(Place::Beside.path(&wanted), bytes)]);
    assert!(published(&one, Some(&exact), &wanted, deadline()).unwrap());

    const CHILD: &str = "ATERM_VERIFY_LITERAL_JOURNAL_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let mut child = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "judged::tests::publication_matches_literal_paths_under_inherited_pathspec_modes",
                "--nocapture",
            ])
            .env(CHILD, "1");
        for mode in [
            "GIT_LITERAL_PATHSPECS",
            "GIT_GLOB_PATHSPECS",
            "GIT_NOGLOB_PATHSPECS",
            "GIT_ICASE_PATHSPECS",
        ] {
            child.env(mode, "1");
        }
        let out = aterm_uds::deadline::output_while_process_progressing_complete_with_limit(
            &mut child,
            aterm_uds::deadline::ECHO_HANG,
            "literal journal pathspec child",
            1024 * 1024,
        )
        .unwrap();
        assert!(
            out.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn quoted_lane_paths_prune_the_stale_attempt_and_keep_the_fresh_slot() {
    let (_tmp, one) = scratch(&["origin"]);
    let day = 20_404 * 86_400;
    let fresh = lane_record("t", "held", "pass", day);
    assert!(append(&one, "origin", None, &fresh, deadline()).unwrap());
    let parent = head(&one, "origin", deadline()).unwrap();
    let stale = lane_record("t", "late\t雪\n", "red", day - RETENTION - 86_400);
    assert!(
        append_at(
            &one,
            "origin",
            parent.as_deref(),
            &stale,
            Place::Beside,
            deadline()
        )
        .unwrap()
    );
    let parent = head(&one, "origin", deadline()).unwrap();
    let listed = filed(&one, parent.as_deref(), "verdicts/", deadline()).unwrap();
    assert!(
        listed.contains(&(Place::Beside.path(&stale), stale.clone())),
        "{listed:?}"
    );
    let (blobs, unread) = blobs_under(
        &one,
        parent.as_deref().unwrap(),
        &["verdicts/".into()],
        deadline(),
    )
    .unwrap();
    assert!(unread.is_empty());
    assert!(blobs.contains(&(
        Place::Beside.path(&stale),
        aterm_toml::to_string(&stale).unwrap().into_bytes()
    )));
    let next = lane_record("u", "next", "judging", day + 86_400);
    assert!(append(&one, "origin", parent.as_deref(), &next, deadline()).unwrap());
    let after = head(&one, "origin", deadline()).unwrap();
    assert_eq!(
        record(&one, after.as_deref(), "t", deadline()).unwrap(),
        Some(fresh.clone())
    );
    let remaining = records(&one, after.as_deref(), deadline()).unwrap();
    assert_eq!(remaining.len(), 2);
    assert!(
        remaining.contains(&fresh) && remaining.contains(&next),
        "{remaining:?}"
    );
}

#[test]
fn journal_listings_preserve_paths_and_require_complete_utf8_records() {
    assert_eq!(journal_listing(b"").unwrap().count(), 0);
    let paths = [
        " leading and trailing ",
        "line\nbreak",
        "tab\there",
        "雪",
        "�",
    ];
    let listing = paths
        .iter()
        .map(|path| format!("{path}\0"))
        .collect::<String>();
    assert_eq!(
        journal_listing(listing.as_bytes())
            .unwrap()
            .collect::<Vec<_>>(),
        paths
    );
    for invalid in [
        &b"missing terminator"[..],
        b"complete\0partial",
        b"\0",
        b"first\0\0last\0",
        b"first\0\0",
        b"invalid-\xff\0",
    ] {
        assert!(journal_listing(invalid).is_err(), "{invalid:?}");
    }
}

#[test]
fn journal_batch_requires_matching_objects_bounded_bodies_and_exact_framing() {
    let first = "1111111111111111111111111111111111111111";
    let second = "2222222222222222222222222222222222222222";
    let requested = vec![
        ("first".into(), first.into()),
        ("second".into(), second.into()),
    ];
    let valid = format!("{first} blob 3\na\0b\n{second} missing\n");
    let (read, unread) = parse_blobs(&requested, valid.as_bytes()).unwrap();
    assert_eq!(read, vec![("first".into(), b"a\0b".to_vec())]);
    assert_eq!(
        unread,
        vec![("second".into(), "its object is missing".into())]
    );
    let one = &requested[..1];
    for invalid in [
        format!("{second} blob 0\n\n"),
        format!("{second} missing\n"),
        format!("{first} blob 2\nx"),
        format!("{first} blob 1\nx!"),
        format!("{first} blob 0\n\nextra"),
        format!("{first} missing\nextra"),
        format!("{first} tree 0\n\n"),
        format!("{first} blob 0"),
    ] {
        assert!(parse_blobs(one, invalid.as_bytes()).is_err(), "{invalid:?}");
    }
    assert!(parse_blobs(&requested, format!("{first} blob 0\n\n").as_bytes()).is_err());
    let body = vec![b'x'; crate::land_record::MAX_BYTES as usize];
    let mut response = format!("{first} blob {}\n", body.len()).into_bytes();
    response.extend_from_slice(&body);
    response.push(b'\n');
    assert_eq!(
        parse_blobs(one, &response).unwrap().0,
        vec![("first".into(), body)]
    );
    let mut oversized =
        format!("{first} blob {}\n", crate::land_record::MAX_BYTES + 1).into_bytes();
    oversized.extend(std::iter::repeat_n(
        b'x',
        crate::land_record::MAX_BYTES as usize + 1,
    ));
    oversized.push(b'\n');
    assert!(parse_blobs(one, &oversized).is_err());
}

#[test]
fn journal_timeout_does_not_spawn_after_the_inherited_budget_expires() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let sentinel = "journal-argument-and-input-sentinel";
    // A spawn would fail with NotFound. Expiry must instead retain the owned-input
    // pre-start facts without running or naming this nonexistent command.
    for watching in [false, true] {
        let check = || {
            let mut cmd = aterm_uds::spawnfd::command(dir.path().join(sentinel));
            cmd.arg(sentinel);
            let expired = std::time::Instant::now() - std::time::Duration::from_secs(1);
            let why = capture_until(&mut cmd, Some(sentinel.as_bytes()), expired).unwrap_err();
            assert!(why.contains("inherited_remaining_ms=0"), "{why}");
            assert!(why.contains("return_elapsed_ms="), "{why}");
            assert!(!why.contains(sentinel), "{why}");
            why
        };
        let why = if watching {
            interruptible(check)
        } else {
            check()
        };
        assert!(why.contains("not-started"), "{why}");
    }
}

#[test]
fn journal_mutation_admission_expires_but_an_admitted_writer_owns_its_full_window() {
    let now = std::time::Instant::now();
    let read_until = now + std::time::Duration::from_secs(60);
    let budget = AppendBudget::new(read_until);
    assert_eq!(budget.deadline(), read_until);
    assert!(
        budget
            .admit(now + std::time::Duration::from_secs(60))
            .is_err()
    );
    let admitted_at = now + std::time::Duration::from_secs(59);
    let MutationBudget(until) = budget.admit(admitted_at).unwrap();
    assert_eq!(until.duration_since(admitted_at), agh::git::GRANT_MAX);
    assert!(until > read_until);
    assert_eq!(
        budget.deadline(),
        until,
        "race readback gets the admitted work window"
    );
    let later = read_until + std::time::Duration::from_secs(1);
    let MutationBudget(second) = budget.admit(later).unwrap();
    assert_eq!(
        second, until,
        "a race must not renew the publication window"
    );
    let MutationBudget(last) = budget
        .admit(until - std::time::Duration::from_nanos(1))
        .unwrap();
    assert_eq!(last, until);
    assert!(budget.admit(until).is_err());
    assert!(
        budget
            .admit(until + std::time::Duration::from_secs(1))
            .is_err()
    );
    assert_eq!(budget.deadline(), until, "expiry cannot reset admission");
}

#[test]
fn journal_timeout_read_context_survives_a_transport_failure() {
    use std::cell::Cell;
    let dir = aterm_tempfile::tempdir().unwrap();
    let sentinel = "journal-remote-name-sentinel";
    let mut diagnostics = std::collections::BTreeSet::new();
    for context in [
        ReadContext::Read,
        ReadContext::AppendHead,
        ReadContext::AfterPushFailure,
    ] {
        let calls = Cell::new(0);
        let run = |_, _: &[std::ffi::OsString]| {
            calls.set(calls.get() + 1);
            Err("controlled transport failure".into())
        };
        let why = read_head(dir.path(), sentinel, context, &run).unwrap_err();
        assert_eq!(calls.get(), 1, "a failure starts no later read");
        assert!(why.ends_with("controlled transport failure"), "{why}");
        assert!(!why.contains(sentinel), "{why}");
        diagnostics.insert(why);
    }
    assert_eq!(
        diagnostics.len(),
        3,
        "ordinary reads, append heads and post-push rechecks must remain distinguishable"
    );
}

#[test]
fn journal_timeout_head_retains_an_already_exhausted_deadline() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let expired = std::time::Instant::now() - std::time::Duration::from_secs(1);
    for context in [
        ReadContext::Read,
        ReadContext::AppendHead,
        ReadContext::AfterPushFailure,
    ] {
        let why = head_in(dir.path(), "origin", expired, context).unwrap_err();
        assert!(
            why.contains("inherited_remaining_ms=0"),
            "a journal read must not renew its inherited budget: {why}"
        );
        assert!(why.contains("not-started"), "{why}");
    }
}

#[test]
fn github_journal_push_uses_agh_and_keeps_refusal_evidence_without_unknown_retries() {
    use crate::remote::Tool;
    use std::cell::Cell;
    use std::os::unix::process::ExitStatusExt as _;

    let dir = aterm_tempfile::tempdir().unwrap();
    let parent = "1111111111111111111111111111111111111111";
    let sha = "2222222222222222222222222222222222222222";
    for code in [0, 9, 13] {
        let pushes = Cell::new(0);
        let reads = Cell::new(0);
        let output = |code: i32, stdout: &[u8], stderr: &[u8]| std::process::Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: stdout.to_vec(),
            stderr: stderr.to_vec(),
        };
        let read = |tool, args: &[std::ffi::OsString]| match tool {
            Tool::Git => {
                let words: Vec<_> = args.iter().map(|arg| arg.to_str().unwrap()).collect();
                assert!(
                    words == ["remote", "get-url", "--all", "origin"]
                        || words == ["remote", "get-url", "--all", "--push", "origin"],
                    "GitHub journal transport must not fall back to git: {words:?}"
                );
                Ok(output(0, b"https://github.com/fixture/repo.git\n", b""))
            }
            Tool::Agh => {
                assert_ne!(
                    args.get(1).and_then(|word| word.to_str()),
                    Some("push"),
                    "the read runner must never own publication"
                );
                reads.set(reads.get() + 1);
                Err("controlled head lookup failure".to_owned())
            }
        };
        let write = |tool, args: &[std::ffi::OsString]| {
            assert_eq!(tool, Tool::Agh);
            assert_eq!(pushes.replace(pushes.get() + 1), 0);
            assert_eq!(args[0], "repo");
            assert_eq!(args[1], "push");
            assert_eq!(args[2], "fixture/repo");
            let input = args.iter().position(|word| word == "--input").unwrap();
            let batch: aterm_json::Value =
                aterm_json::from_slice(&std::fs::read(Path::new(&args[input + 1])).unwrap())
                    .unwrap();
            assert_eq!(
                batch,
                aterm_json::json!([{"ref":REF,"before":parent,"after":sha}])
            );
            Ok(output(code, b"", b"original push refusal"))
        };
        let result = push_journal(
            dir.path(),
            "origin",
            Some(parent),
            sha,
            &read,
            crate::remote::WriteRunner(&write),
        );
        assert_eq!(pushes.get(), 1);
        if let Err(why) = &result {
            assert!(why.contains(&format!("journal_ref={REF}")), "{why}");
            assert!(why.contains(&format!("journal_before={parent}")), "{why}");
            assert!(why.contains(&format!("journal_after={sha}")), "{why}");
        }
        match code {
            0 => {
                assert_eq!(result, Ok(Appended::Holds(sha.to_owned())));
                assert_eq!(reads.get(), 0);
            }
            9 => {
                let why = result.unwrap_err();
                assert!(why.contains("original push refusal"), "{why}");
                assert!(why.contains("controlled head lookup failure"), "{why}");
                assert!(why.contains("journal-after-push-failure"), "{why}");
                assert_eq!(reads.get(), 1);
            }
            13 => {
                let why = result.unwrap_err();
                assert!(why.contains("outcome unknown"), "{why}");
                assert_eq!(reads.get(), 0, "unknown sends are never retried or raced");
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn journal_append_reuses_each_races_imported_head_including_a_removed_ref() {
    let mut reads = 0;
    let mut parents = Vec::new();
    let outcomes = [
        Appended::Raced(Some("moved".to_owned())),
        Appended::Raced(None),
        Appended::Holds("landed".to_owned()),
    ];
    let mut outcomes = outcomes.into_iter();
    let result = append_land_through(
        false,
        || {
            reads += 1;
            Ok(Some("initial".to_owned()))
        },
        |parent| {
            parents.push(parent.map(str::to_owned));
            Ok(outcomes.next().expect("only three attempts"))
        },
    );
    assert_eq!(result, Ok(Some("landed".to_owned())));
    assert_eq!(
        parents,
        [Some("initial".to_owned()), Some("moved".to_owned()), None]
    );
    assert_eq!(reads, 1, "a race already imported its observed head");
}

#[test]
fn journal_append_stops_on_an_unknown_write_after_a_race() {
    let mut reads = 0;
    let mut parents = Vec::new();
    let why = append_land_through(
        false,
        || {
            reads += 1;
            assert_eq!(reads, 1, "no fresh observation may authorize a retry");
            Ok(Some("initial".to_owned()))
        },
        |parent| {
            parents.push(parent.map(str::to_owned));
            match parents.len() {
                1 => Ok(Appended::Raced(Some("moved".to_owned()))),
                2 => Err("controlled unknown write outcome".to_owned()),
                _ => panic!("an unknown write must not be attempted again"),
            }
        },
    )
    .unwrap_err();
    assert!(why.contains("controlled unknown write outcome"), "{why}");
    assert_eq!(
        parents,
        [Some("initial".to_owned()), Some("moved".to_owned())]
    );
    assert_eq!(reads, 1);
}

#[test]
fn a_journal_race_reuses_written_blobs_but_rebuilds_over_the_moved_head() {
    use crate::checks::Check;
    use crate::land_record::Word;
    use crate::land_record::tests::{BASE, NOW, T, record, signer};
    use std::cell::Cell;
    let (_tmp, root) = scratch(&["origin"]);
    let me = signer(7);
    let rival = record(Check::Lint, Word::Ok, T, BASE, NOW)
        .sign(Some(&me))
        .unwrap();
    let mine = record(Check::Fmt, Word::Ok, T, BASE, NOW)
        .sign(Some(&me))
        .unwrap();
    append_land(
        &root,
        "origin",
        std::slice::from_ref(&rival),
        NOW,
        deadline(),
    )
    .unwrap();
    let budget = AppendBudget::new(deadline());
    let writes = Cell::new(0);
    let mut blob = cached_blobs(|bytes| {
        writes.set(writes.get() + 1);
        write_blob(&root, bytes, budget.deadline())
    });
    let mut attempts = 0;
    let head = append_land_through(
        false,
        || Ok(None),
        |parent| {
            attempts += 1;
            land_append_with(
                &root,
                "origin",
                parent,
                std::slice::from_ref(&mine),
                NOW,
                &budget,
                &mut blob,
            )
        },
    )
    .unwrap()
    .unwrap();
    assert_eq!(attempts, 2);
    assert_eq!(bytes_at(&root, &head, &rival.path), rival.bytes);
    assert_eq!(bytes_at(&root, &head, &mine.path), mine.bytes);
    assert_eq!(
        writes.get(),
        1,
        "the rejected attempt already wrote this blob"
    );
    let changed = blob(b"different bytes").unwrap();
    assert_ne!(changed, blob(&mine.bytes).unwrap());
    assert_eq!(
        writes.get(),
        2,
        "only different bytes require another write"
    );
}

#[test]
fn lane_batches_require_every_requested_object_in_order() {
    let first = "1".repeat(40);
    let second = "2".repeat(40);
    let frame = |id: &str, body: &str| format!("{id} blob {}\n{body}\n", body.len());
    let a = frame(&first, "a\n雪");
    let b = frame(&second, "");
    let both = format!("{a}{b}");
    assert_eq!(
        blobs(both.as_bytes(), &[&first, &second]).unwrap(),
        ["a\n雪", ""]
    );
    assert_eq!(
        blobs(format!("{a}{a}").as_bytes(), &[&first, &first]).unwrap(),
        ["a\n雪", "a\n雪"]
    );
    assert!(blobs(b"", &[]).unwrap().is_empty());
    for (answer, requested) in [
        (both.as_bytes(), vec![second.as_str(), first.as_str()]),
        (a.as_bytes(), vec![second.as_str()]),
        (a.as_bytes(), vec![first.as_str(), second.as_str()]),
        (both.as_bytes(), vec![first.as_str()]),
        (a.as_bytes(), vec![]),
        (b"".as_slice(), vec![first.as_str()]),
        (&a.as_bytes()[..a.len() - 1], vec![first.as_str()]),
    ] {
        assert!(
            blobs(answer, &requested).is_err(),
            "{answer:?} for {requested:?}"
        );
    }
}

#[test]
fn journal_history_includes_roots_hidden_by_local_log_settings() {
    let (_tmp, root) = scratch(&["origin"]);
    let held = lane_record("t", "held", "pass", 42);
    assert!(append(&root, "origin", None, &held, deadline()).unwrap());
    let parent = head(&root, "origin", deadline()).unwrap().unwrap();
    git(&root, &["config", "log.showRoot", "false"]).unwrap();
    assert!(
        git(&root, &["log", "--format=", "--raw", &parent])
            .unwrap()
            .is_empty(),
        "negative control: local log settings hide the root diff"
    );
    assert_eq!(history(&root, &parent, 0).unwrap(), [held.clone()]);
    assert!(published(&root, Some(&parent), &held, deadline()).unwrap());
}

#[test]
fn journal_identity_ignores_local_object_replacements() {
    let (_tmp, root) = scratch(&["origin"]);
    let held = lane_record("t", "held", "pass", 42);
    let never = lane_record("t", "never", "red", 42);
    assert!(append(&root, "origin", None, &held, deadline()).unwrap());
    let parent = head(&root, "origin", deadline()).unwrap().unwrap();
    let path = format!("{parent}:verdicts/t.toml");
    let original = rev(&root, &path);
    let replacement = blob_of(&root, &aterm_toml::to_string(&never).unwrap());
    git(&root, &["replace", &original, &replacement]).unwrap();
    let mut raw = crate::git_provider::command();
    raw.current_dir(&root)
        .args(["show", &path])
        .env_remove("GIT_NO_REPLACE_OBJECTS");
    let replaced: Record = aterm_toml::from_str(&fixture(&mut raw, None).unwrap()).unwrap();
    assert_eq!(
        replaced, never,
        "negative control: ordinary Git sees the replacement"
    );
    assert_eq!(
        records(&root, Some(&parent), deadline()).unwrap(),
        [held.clone()]
    );
    assert_eq!(
        record(&root, Some(&parent), "t", deadline()).unwrap(),
        Some(held.clone())
    );
    assert_eq!(history(&root, &parent, 0).unwrap(), [held.clone()]);
    assert!(published(&root, Some(&parent), &held, deadline()).unwrap());
    assert!(!published(&root, Some(&parent), &never, deadline()).unwrap());
}

#[test]
fn journal_commands_stay_in_the_requested_repository() {
    const CHILD: &str = "ATERM_VERIFY_JOURNAL_REPOSITORY_CHILD";
    if let Some(root) = std::env::var_os(CHILD) {
        let root = std::path::PathBuf::from(root).canonicalize().unwrap();
        let mut raw = crate::git_provider::command();
        raw.current_dir(&root)
            .args(["rev-parse", "--absolute-git-dir"]);
        let redirected = fixture(&mut raw, None).unwrap();
        assert_ne!(
            std::path::Path::new(redirected.trim()),
            root.join(".git"),
            "negative control: ordinary Git follows the inherited directory"
        );
        assert_eq!(
            std::path::Path::new(&git(&root, &["rev-parse", "--absolute-git-dir"]).unwrap()),
            root.join(".git")
        );
        let held = lane_record("t", "held", "pass", 42);
        let parent = head(&root, "origin", deadline()).unwrap().unwrap();
        assert_eq!(records(&root, Some(&parent), deadline()).unwrap(), [held]);
        write_blob(&root, b"journal repository custody sentinel", deadline()).unwrap();
        return;
    }
    let (_tmp, root) = scratch(&["origin"]);
    let held = lane_record("t", "held", "pass", 42);
    assert!(append(&root, "origin", None, &held, deadline()).unwrap());
    let foreign = root.parent().unwrap().join("origin");
    let mut child = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    child
        .args([
            "--exact",
            "judged::tests::journal_commands_stay_in_the_requested_repository",
            "--nocapture",
        ])
        .env(CHILD, &root)
        .env("GIT_DIR", &foreign)
        .env("GIT_WORK_TREE", &foreign)
        .env("GIT_OBJECT_DIRECTORY", foreign.join("objects"));
    let out = aterm_uds::deadline::output_while_process_progressing_complete_with_limit(
        &mut child,
        aterm_uds::deadline::ECHO_HANG,
        "journal repository child",
        1024 * 1024,
    )
    .unwrap();
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let id = fixture(
        &mut command(&root, &["hash-object", "--stdin"]),
        Some(b"journal repository custody sentinel"),
    )
    .unwrap();
    git(&root, &["cat-file", "-e", id.trim()]).unwrap();
    assert!(
        git(&foreign, &["cat-file", "-e", id.trim()]).is_err(),
        "the journal write must not reach the inherited object store"
    );
}

#[test]
fn journal_reads_do_not_lazy_fetch_missing_promisor_objects() {
    let (_tmp, root) = scratch(&["origin"]);
    let held = lane_record("t", "held", "pass", 42);
    assert!(append(&root, "origin", None, &held, deadline()).unwrap());
    let parent = head(&root, "origin", deadline()).unwrap().unwrap();
    let id = rev(&root, &format!("{parent}:verdicts/t.toml"));
    let foreign = root.parent().unwrap().join("origin");
    git(&foreign, &["config", "uploadpack.allowFilter", "true"]).unwrap();
    git(
        &foreign,
        &["config", "uploadpack.allowAnySHA1InWant", "true"],
    )
    .unwrap();
    git(&root, &["config", "remote.origin.promisor", "true"]).unwrap();
    git(
        &root,
        &["config", "remote.origin.partialclonefilter", "blob:none"],
    )
    .unwrap();
    // Only this fixture's loose blob is removed; its local promisor still holds it.
    std::fs::remove_file(root.join(".git/objects").join(&id[..2]).join(&id[2..])).unwrap();
    assert!(
        records(&root, Some(&parent), deadline()).is_err(),
        "a local journal read must report the absent object, not fetch it"
    );
    let mut raw = crate::git_provider::command();
    raw.current_dir(&root)
        .args(["cat-file", "blob", &id])
        .env_remove("GIT_NO_LAZY_FETCH");
    let restored: Record = aterm_toml::from_str(&fixture(&mut raw, None).unwrap()).unwrap();
    assert_eq!(
        restored, held,
        "negative control: ordinary Git fetches the missing blob from the local promisor"
    );
    assert_eq!(records(&root, Some(&parent), deadline()).unwrap(), [held]);
}
