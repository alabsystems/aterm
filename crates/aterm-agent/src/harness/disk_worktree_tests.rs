// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The worktree step on a scratch home laid out as agents leave one: a main checkout `aterm`
//! with a submodule, `origin/main` at its HEAD, and linked worktrees where agents make them,
//! `Developer/.ubd/aterm-<name>.noindex`, each on its own branch `agent/<name>` with its
//! submodule populated. Git is the real one; the
//! process probe is injected (inert, or naming one worktree as a running build's). "Now" is a
//! week and a day after the scratch was laid, so what the test writes is old unless it says
//! otherwise. Every case where a worktree stays carries a control worktree, merged and clean,
//! that goes in the same step: a step that did nothing would fail it.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use aterm_json::Value;
use aterm_spec::wait::MUST_HAPPEN;
use aterm_uds::holders::{PathScan, PathUse};
use atpkg::build_cache::retired::{self, Git as _, GitCli, UNWRITTEN};

use super::{Done, Facts, Hooks, OpenSession, Schedule, step};
use crate::harness::disk::Refusal;
use crate::harness::disk::idle::SessionCwd;

const DAY: Duration = Duration::from_secs(86_400);

/// A scratch home, removed when the test ends.
struct Scratch {
    _dir: aterm_tempfile::TempDir,
    home: PathBuf,
    now: SystemTime,
}

impl Scratch {
    fn checkout(&self) -> PathBuf {
        self.home.join("aterm")
    }

    fn transcripts(&self) -> PathBuf {
        self.home.join(".claude").join("projects")
    }

    /// A linked worktree where an agent makes one, `Developer/.ubd/aterm-<name>.noindex`, on a
    /// new branch `agent/<name>` at the checkout's HEAD, its submodule populated.
    fn wt(&self, name: &str) -> PathBuf {
        let wt = self
            .home
            .join("Developer")
            .join(".ubd")
            .join(format!("aterm-{name}.noindex"));
        git(
            &self.checkout(),
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                &format!("agent/{name}"),
                &wt.to_string_lossy(),
                "HEAD",
            ],
        );
        git(&wt, &["submodule", "update", "-q", "--init"]);
        wt
    }

    /// Whether git lists `wt` as a worktree of the checkout.
    fn listed(&self, wt: &Path) -> bool {
        let list = GitCli.run(&self.checkout(), &["worktree", "list", "--porcelain"]);
        assert_eq!(list.code, Some(0), "{}", list.err);
        list.out
            .lines()
            .any(|l| l.strip_prefix("worktree ") == Some(&*wt.to_string_lossy()))
    }

    /// The commit `rev` names in `dir`, `None` when it names none.
    fn rev(&self, dir: &Path, rev: &str) -> Option<String> {
        let a = GitCli.run(dir, &["rev-parse", "--verify", "--quiet", rev]);
        (a.code == Some(0)).then(|| a.out.trim().to_owned())
    }

    /// Set `path`'s modification time to `ago` before this test's now.
    fn written(&self, path: &Path, ago: Duration) {
        std::fs::File::open(path)
            .and_then(|f| f.set_modified(self.now - ago))
            .expect("the time set");
    }

    /// The receipts an interrupted removal left in the checkout's git directory.
    fn receipts(&self) -> Vec<PathBuf> {
        std::fs::read_dir(self.checkout().join(".git/aterm-retiring"))
            .map(|d| {
                d.flatten()
                    .map(|e| e.path())
                    .filter(|p| p.is_file())
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// `git <args>` in `dir`, as a person set the scratch up: it must succeed.
#[track_caller]
fn git(dir: &Path, args: &[&str]) {
    let mut all = vec![
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@example.invalid",
        "-c",
        "commit.gpgsign=false",
        "-c",
        "protocol.file.allow=always",
        "-c",
        "init.defaultBranch=main",
    ];
    all.extend_from_slice(args);
    let a = GitCli.run(dir, &all);
    assert_eq!(
        a.code,
        Some(0),
        "git {args:?} in {}: {}",
        dir.display(),
        a.err
    );
}

fn scratch() -> Scratch {
    let dir = aterm_tempfile::Builder::new()
        .prefix("disk-worktree-")
        .suffix(".noindex")
        .tempdir()
        .expect("scratch");
    let home = std::fs::canonicalize(dir.path())
        .expect("resolved")
        .join("home");
    let sub = home.join("astream");
    let checkout = home.join("aterm");
    for repo in [&sub, &checkout] {
        std::fs::create_dir_all(repo).expect("a repo");
        git(repo, &["init", "-q"]);
    }
    std::fs::write(sub.join("lib.rs"), "// sub\n").expect("written");
    git(&sub, &["add", "lib.rs"]);
    git(&sub, &["commit", "-qm", "sub"]);
    std::fs::write(checkout.join("Cargo.toml"), "[workspace]\n").expect("written");
    std::fs::write(checkout.join(".gitignore"), "/target*\n/.claude/\n").expect("written");
    git(&checkout, &["add", "Cargo.toml", ".gitignore"]);
    git(
        &checkout,
        &[
            "submodule",
            "add",
            "-q",
            &sub.to_string_lossy(),
            "vendor/astream",
        ],
    );
    git(&checkout, &["commit", "-qm", "main"]);
    git(
        &checkout,
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    Scratch {
        _dir: dir,
        home,
        now: SystemTime::now() + UNWRITTEN + DAY,
    }
}

/// What one step is run with beyond the scratch.
#[derive(Default)]
struct Setup {
    cwds: Vec<PathBuf>,
    cwd_unknown: Option<String>,
    open: Vec<OpenSession>,
    /// A worktree a running build uses, as the process probe reports it.
    busy: Option<PathBuf>,
    /// Candidates beyond what discovery finds.
    extra: Vec<(PathBuf, PathBuf)>,
    /// The ledger refuses every row.
    journal_off: bool,
}

/// One step over what discovery finds in the scratch home (and `setup.extra`), with the real
/// remover and the injected probe: what it did, and the rows it put on record.
fn run(s: &Scratch, setup: &Setup) -> (Done, Vec<String>) {
    let mut found = crate::harness::disk::discover::linked_worktrees(&s.home);
    found.extend(setup.extra.iter().cloned());
    let known: Vec<PathBuf> = found
        .iter()
        .flat_map(|(c, w)| [c.clone(), w.clone()])
        .collect();
    let cwds: Vec<SessionCwd> = setup.cwds.iter().map(|c| SessionCwd::on(c, None)).collect();
    let transcripts = s.transcripts();
    let facts = Facts {
        home: &s.home,
        transcripts_root: Some(&transcripts),
        cwds: &cwds,
        cwd_unknown: setup.cwd_unknown.as_deref(),
        open: &setup.open,
        table: None,
        known: &known,
    };
    let rows = RefCell::new(Vec::new());
    let busy = setup.busy.clone();
    let probe = move |dirs: &[PathBuf], _: &[u32]| -> std::io::Result<PathScan> {
        let users = match &busy {
            Some(b) if dirs.iter().any(|d| b.starts_with(d)) => vec![PathUse {
                pid: 7,
                how: "cwd",
                path: b.clone(),
            }],
            _ => Vec::new(),
        };
        Ok(PathScan {
            users,
            ..PathScan::default()
        })
    };
    let mut remove = |checkout: &Path, wt: &Path| {
        retired::remove_worktree_with_processes(&GitCli, &s.home, checkout, wt, s.now, &probe)
    };
    let mut journal = |row: &str| {
        if setup.journal_off {
            return false;
        }
        rows.borrow_mut().push(row.to_owned());
        true
    };
    let still_held = || true;
    // Judging may take half the wall: the step's own split leaves each phase a one-minute hang
    // detector.
    let (done, _) = step(
        &GitCli,
        &facts,
        &found,
        None,
        s.now,
        Instant::now() + MUST_HAPPEN * 2,
        Hooks {
            still_held: &still_held,
            remove: &mut remove,
            journal: &mut journal,
        },
    );
    assert_eq!(done.unfinished, None, "the step ran through: {done:#?}");
    (done, rows.into_inner())
}

/// Why `wt` stayed in `done`.
#[track_caller]
fn kept_for(done: &Done, wt: &Path) -> String {
    done.kept
        .iter()
        .find(|(p, _)| p == wt)
        .map(|(_, why)| why.clone())
        .unwrap_or_else(|| panic!("{} was not kept: {done:#?}", wt.display()))
}

/// Whether `wt` went in `done`.
fn went(done: &Done, wt: &Path) -> bool {
    done.removed.iter().any(|r| r.path == wt) && wt.symlink_metadata().is_err()
}

/// The rows of kind `worktree`, parsed.
fn worktree_rows(rows: &[String]) -> Vec<Value> {
    rows.iter()
        .map(|r| aterm_json::from_str::<Value>(r).expect("a JSON row"))
        .filter(|v| v["kind"].as_str() == Some(crate::harness::disk::WORKTREE_ROW_KIND))
        .collect()
}

/// A MERGED, CLEAN WORKTREE GOES, WITH ITS MERGED BRANCH: a worktree whose commit `origin/main`
/// took by fast-forward goes, its populated submodule with it, and its branch is deleted; the
/// checkout and its submodule store stay, no receipt is left, the ledger holds the intent and
/// then the outcome, and its old build folder is left for the cache pass (`Unused::Gone`). With
/// the ledger refusing rows, nothing of it is touched. NEGATIVE CONTROLS: without the
/// write-ahead check the first run removes it; without the branch deletion its branch stays.
#[test]
fn a_merged_clean_worktree_goes_with_its_merged_branch() {
    let s = scratch();
    let wt = s.wt("done");
    std::fs::write(wt.join("x.rs"), "\n").expect("written");
    git(&wt, &["add", "x.rs"]);
    git(&wt, &["commit", "-qm", "x"]);
    let x = s.rev(&wt, "HEAD").expect("its commit");
    git(
        &s.checkout(),
        &["update-ref", "refs/remotes/origin/main", &x],
    );
    let mirror = atpkg::build_cache::build_dir(&atpkg::build_cache::cache_root(&s.home), &wt)
        .join("debug")
        .join("f");
    std::fs::create_dir_all(mirror.parent().expect("its folder")).expect("a build folder");
    std::fs::write(&mirror, "x").expect("written");

    let (done, rows) = run(
        &s,
        &Setup {
            journal_off: true,
            ..Setup::default()
        },
    );
    assert!(wt.join("x.rs").is_file(), "unjournalled, it stands");
    assert_eq!(
        s.rev(&s.checkout(), "refs/heads/agent/done"),
        Some(x.clone())
    );
    assert_eq!(
        done.refused,
        [Refusal::Unjournalled(wt.clone())],
        "{done:#?}"
    );
    assert!(done.removed.is_empty() && rows.is_empty(), "{done:#?}");

    let (done, rows) = run(&s, &Setup::default());
    assert!(went(&done, &wt), "{done:#?}");
    assert!(!s.listed(&wt), "git no longer lists it");
    assert_eq!(
        s.rev(&s.checkout(), "refs/heads/agent/done"),
        None,
        "its branch went"
    );
    assert_eq!(done.removed[0].branch_deleted, Ok(()));
    assert!(s.checkout().join(".git/modules/vendor/astream").is_dir());
    assert!(s.checkout().join("Cargo.toml").is_file());
    assert_eq!(s.receipts(), Vec::<PathBuf>::new());
    let rows = worktree_rows(&rows);
    let actions: Vec<&str> = rows.iter().filter_map(|r| r["action"].as_str()).collect();
    assert_eq!(actions, ["removing", "removed"], "{rows:#?}");
    assert_eq!(rows[1]["branch_deleted"].as_bool(), Some(true), "{rows:#?}");
    assert_eq!(rows[1]["branch"].as_str(), Some("refs/heads/agent/done"));
    assert!(mirror.is_file(), "the build folder is the cache pass's");
}

/// A WORKTREE WRITTEN WITHIN SEVEN DAYS STAYS: a tracked file written six days ago, its build
/// folder written yesterday, and git's own reflog of its HEAD written two days ago (a reset or a
/// checkout writes that, not a file in the tree) each keep their worktree, still listed on its
/// branch, by the judge's own words and before any removal began (no receipt), while the
/// control goes. NEGATIVE CONTROLS: without the tree walk A, and without git's own files counted
/// D, reach the remover, whose custody check refuses them in other words ("written after
/// snapshot removal began": this test's now lies a week ahead of the clock the remover dates its
/// receipt by); without the build-folder clause B goes.
#[test]
fn a_worktree_written_within_seven_days_stays() {
    let s = scratch();
    let a = s.wt("a");
    let b = s.wt("b");
    let d = s.wt("d");
    let c = s.wt("c");
    s.written(&a.join("Cargo.toml"), 6 * DAY);
    let mirror = atpkg::build_cache::build_dir(&atpkg::build_cache::cache_root(&s.home), &b)
        .join("debug")
        .join("f");
    std::fs::create_dir_all(mirror.parent().expect("its folder")).expect("a build folder");
    std::fs::write(&mirror, "x").expect("written");
    s.written(&mirror, DAY);
    let entry = std::fs::read_to_string(d.join(".git")).expect("its .git");
    let entry = PathBuf::from(
        entry
            .trim()
            .strip_prefix("gitdir:")
            .expect("a gitfile")
            .trim(),
    );
    s.written(&entry.join("logs").join("HEAD"), 2 * DAY);

    let (done, _) = run(&s, &Setup::default());
    assert!(went(&done, &c), "the control goes: {done:#?}");
    let judged = "; it goes once nothing in it is written for";
    let why = kept_for(&done, &a);
    assert!(
        why.contains("Cargo.toml was written") && why.contains(judged),
        "{why}"
    );
    let why = kept_for(&done, &b);
    assert!(why.contains("its build folder"), "{why}");
    let why = kept_for(&done, &d);
    assert!(
        why.contains("logs/HEAD was written") && why.contains(judged),
        "{why}"
    );
    assert_eq!(s.receipts(), Vec::<PathBuf>::new(), "no removal began");
    for (wt, name) in [(&a, "a"), (&b, "b"), (&d, "d")] {
        assert!(s.listed(wt), "{name} is listed");
        assert!(
            s.rev(&s.checkout(), &format!("refs/heads/agent/{name}"))
                .is_some(),
            "{name}'s branch stays"
        );
    }
}

/// A WORKTREE AN OPEN SESSION'S TRANSCRIPT NAMES STAYS: one named by its full path, one by
/// `~/…`, one by `../…` (as an agent in the main checkout writes it), one only a subagent's
/// transcript names (`<session>/subagents/*.jsonl`; a subagent works in its session's folder),
/// and one only a workflow agent's names (`<session>/subagents/workflows/<run>/*.jsonl`), while
/// the control goes; and while any open session's transcript cannot be named, nothing
/// goes, a fresh control included. NEGATIVE CONTROLS: without the transcript read N goes; with
/// whole paths for names T and REL go; without the subagents' transcripts SUB goes; with only
/// the first level of them WF goes; without the fail-closed rule C2 goes.
#[test]
fn a_worktree_an_open_sessions_transcript_names_stays() {
    let s = scratch();
    let n = s.wt("n");
    let t = s.wt("t");
    let rel = s.wt("rel");
    let sub = s.wt("sub");
    let wf = s.wt("wf");
    let c = s.wt("c");
    let transcript = s.home.join("talk").join("transcript.jsonl");
    std::fs::create_dir_all(transcript.parent().expect("its folder")).expect("a folder");
    std::fs::write(
        &transcript,
        format!(
            "{{\"cwd\":\"/elsewhere\",\"text\":\"cd {}\"}}\n\
             {{\"text\":\"ls ~/Developer/.ubd/aterm-t.noindex\"}}\n\
             {{\"text\":\"git -C ../aterm-rel.noindex status\"}}\n",
            n.display()
        ),
    )
    .expect("written");
    let subagents = s.home.join("talk").join("transcript").join("subagents");
    std::fs::create_dir_all(&subagents).expect("its subagents' folder");
    std::fs::write(
        subagents.join("agent-1.jsonl"),
        "{\"text\":\"cargo test --manifest-path aterm-sub.noindex/Cargo.toml\"}\n",
    )
    .expect("written");
    let run_folder = subagents.join("workflows").join("wf_1");
    std::fs::create_dir_all(&run_folder).expect("a workflow's folder");
    std::fs::write(
        run_folder.join("agent-2.jsonl"),
        "{\"text\":\"git -C aterm-wf.noindex log\"}\n",
    )
    .expect("written");
    let talking = OpenSession {
        pid: 1,
        cwd: Some(s.home.join("x")),
        transcript: Ok(transcript.clone()),
    };
    let (done, _) = run(
        &s,
        &Setup {
            open: vec![talking.clone()],
            ..Setup::default()
        },
    );
    assert!(went(&done, &c), "the control goes: {done:#?}");
    for wt in [&n, &t, &rel, &sub, &wf] {
        let why = kept_for(&done, wt);
        assert!(why.contains("transcript"), "{why}");
        assert!(s.listed(wt), "{}", wt.display());
    }
    let why = kept_for(&done, &sub);
    assert!(why.contains("agent-1.jsonl"), "{why}");
    let why = kept_for(&done, &wf);
    assert!(why.contains("agent-2.jsonl"), "{why}");

    let c2 = s.wt("c2");
    let (done, _) = run(
        &s,
        &Setup {
            open: vec![
                talking,
                OpenSession {
                    pid: 2,
                    cwd: Some(s.home.join("other")),
                    transcript: Err("none".to_owned()),
                },
            ],
            ..Setup::default()
        },
    );
    assert!(done.removed.is_empty(), "{done:#?}");
    let why = kept_for(&done, &c2);
    assert!(why.contains("no readable transcript"), "{why}");
    assert!(s.listed(&c2) && c2.join("Cargo.toml").is_file());
}

/// A WORKTREE A SESSION WORKS IN STAYS: one a session's working directory is inside, and one a
/// running build uses (the process fence, before any receipt is written), each still listed on
/// its branch with its files, while the control goes; and while some session's working
/// directory is unknown, nothing goes. NEGATIVE CONTROLS: without the session clause S goes;
/// with an inert fence P goes.
#[test]
fn a_worktree_a_session_works_in_stays() {
    let s = scratch();
    let sw = s.wt("s");
    let p = s.wt("p");
    let c = s.wt("c");
    let p_branch = s.rev(&s.checkout(), "refs/heads/agent/p");
    let (done, _) = run(
        &s,
        &Setup {
            cwds: vec![sw.join("src")],
            busy: Some(p.clone()),
            ..Setup::default()
        },
    );
    assert!(went(&done, &c), "the control goes: {done:#?}");
    let why = kept_for(&done, &sw);
    assert!(why.contains("a session works in"), "{why}");
    let why = kept_for(&done, &p);
    assert!(
        why.contains("active process use could not be ruled out"),
        "{why}"
    );
    for wt in [&sw, &p] {
        assert!(
            s.listed(wt) && wt.join("Cargo.toml").is_file(),
            "{}",
            wt.display()
        );
    }
    assert_eq!(s.rev(&s.checkout(), "refs/heads/agent/p"), p_branch);
    assert_eq!(
        s.receipts(),
        Vec::<PathBuf>::new(),
        "no receipt was written"
    );

    let c3 = s.wt("c3");
    let (done, _) = run(
        &s,
        &Setup {
            cwd_unknown: Some("a tab reported no working directory".to_owned()),
            ..Setup::default()
        },
    );
    assert!(done.removed.is_empty(), "{done:#?}");
    let why = kept_for(&done, &c3);
    assert!(why.contains("no working directory"), "{why}");
    assert!(s.listed(&c3));
}

/// A WORKTREE WITH EDITS OR UNMERGED COMMITS IS NOT REMOVED BY THE RECEIPT-FREE RULE: a modified
/// tracked file, an untracked file, a commit `origin/main` lacks, a submodule commit the parent
/// does not record, a commit only the reflogs hold, a lock, and the main checkout itself each
/// stay with their reason, every edited byte and every branch as it was and nothing stashed,
/// while the control goes. NEGATIVE CONTROLS: each row's clause removed lets that one go.
#[test]
fn a_worktree_with_edits_or_unmerged_commits_is_not_removed_by_the_receipt_free_rule() {
    let s = scratch();
    let e1 = s.wt("e1");
    std::fs::write(e1.join("Cargo.toml"), "[workspace]\n# mine\n").expect("written");
    let e2 = s.wt("e2");
    std::fs::write(e2.join("notes.txt"), "mine\n").expect("written");
    let e3 = s.wt("e3");
    std::fs::write(e3.join("new.rs"), "\n").expect("written");
    git(&e3, &["add", "new.rs"]);
    git(&e3, &["commit", "-qm", "local"]);
    let e4 = s.wt("e4");
    git(
        &e4.join("vendor/astream"),
        &["commit", "--allow-empty", "-qm", "unrecorded"],
    );
    let e5 = s.wt("e5");
    std::fs::write(e5.join("lost.rs"), "\n").expect("written");
    git(&e5, &["add", "lost.rs"]);
    git(&e5, &["commit", "-qm", "lost"]);
    git(&e5, &["reset", "-q", "--hard", "refs/remotes/origin/main"]);
    let e6 = s.wt("e6");
    git(&s.checkout(), &["worktree", "lock", &e6.to_string_lossy()]);
    let c = s.wt("c");
    let names = ["e1", "e2", "e3", "e4", "e5", "e6"];
    let branches: Vec<Option<String>> = names
        .iter()
        .map(|n| s.rev(&s.checkout(), &format!("refs/heads/agent/{n}")))
        .collect();

    let (done, _) = run(
        &s,
        &Setup {
            extra: vec![(s.checkout(), s.checkout())],
            ..Setup::default()
        },
    );
    assert!(went(&done, &c), "the control goes: {done:#?}");
    for (wt, reason) in [
        (&e1, "uncommitted change"),
        (&e2, "?? notes.txt"),
        (&e3, "is not in origin/main"),
        (&e4, "M vendor/astream"),
        (&e5, "reflog"),
        (&e6, "locked"),
    ] {
        let why = kept_for(&done, wt);
        assert!(why.contains(reason), "{}: {why}", wt.display());
        assert!(s.listed(wt), "{} is listed", wt.display());
    }
    let why = kept_for(&done, &s.checkout());
    assert!(why.contains("a main checkout"), "{why}");
    assert_eq!(
        std::fs::read_to_string(e1.join("Cargo.toml")).expect("its edit"),
        "[workspace]\n# mine\n"
    );
    assert_eq!(
        std::fs::read_to_string(e2.join("notes.txt")).expect("its file"),
        "mine\n"
    );
    let after: Vec<Option<String>> = names
        .iter()
        .map(|n| s.rev(&s.checkout(), &format!("refs/heads/agent/{n}")))
        .collect();
    assert_eq!(after, branches, "every branch as it was");
    let stash = GitCli.run(&s.checkout(), &["stash", "list"]);
    assert_eq!(
        (stash.code, stash.out.as_str()),
        (Some(0), ""),
        "{}",
        stash.err
    );
}

/// `disk.auto_free_gib = 0` TURNS THE STEP OFF: a merged, clean worktree stays, the ledger gets
/// no row, and the step stays due. NEGATIVE CONTROL: without the gate the worktree goes.
#[test]
fn auto_free_gib_zero_removes_no_worktree() {
    let s = scratch();
    let c = s.wt("c");
    let state = s.home.join("state");
    let transcripts = s.transcripts();
    let now = i64::try_from(
        s.now
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("after the epoch")
            .as_secs(),
    )
    .expect("in range");
    let look = crate::harness::cli::WorktreeLook {
        state: &state,
        home: &s.home,
        now,
        transcripts: Some(&transcripts),
        session_cwds: &[],
        session_cwd_unknown: None,
        open: &[],
        table: None,
    };
    let mut schedule = Schedule::new();
    let done = crate::harness::cli::disk_worktree_step(
        &look,
        crate::harness::disk::Config {
            auto_free_gib: 0,
            ..crate::harness::disk::Config::default()
        },
        &mut schedule,
        &|| true,
    );
    assert!(done.removed.is_empty() && done.kept.is_empty(), "{done:#?}");
    assert!(s.listed(&c) && c.join("Cargo.toml").is_file());
    assert!(
        state
            .join(crate::harness::cli::DISK_LEDGER)
            .symlink_metadata()
            .is_err(),
        "no ledger row"
    );
    assert!(schedule.due(Instant::now()), "the step stays due");
}
