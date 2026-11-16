// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The worktree step on a scratch home laid out as agents leave one: a main checkout `aterm`
//! with a submodule, `origin/main` at its HEAD, and linked worktrees where agents make them,
//! `Developer/.ubd/aterm-<name>.noindex`, each on its own branch `agent/<name>` with its
//! submodule populated. Git is the real one; the
//! process probe is injected (inert, or naming one worktree as a running build's). "Now" is a
//! week and a day after the scratch was laid, so what the test writes is old unless it says
//! otherwise. Every case where a worktree stays carries a control worktree, merged and clean,
//! that goes in the same step: a step that did nothing would fail it. A worktree where a person
//! makes one (`aterm-wt-<name>` beside the checkout, on `wt/<name>`) never goes: the cases of
//! it are about its build directory in the shared cache, laid in the scratch home's own cache
//! root, and their control is one whose build directory goes.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use aterm_json::Value;
use aterm_spec::wait::MUST_HAPPEN;
use aterm_uds::holders::{PathScan, PathUse};
use atpkg::build_cache::Unused;
use atpkg::build_cache::retired::{self, Git as _, GitCli, UNWRITTEN};

use super::{
    BUILD_DIR_EVERY, Done, Facts, Hooks, OpenSession, RECORDS_QUIET, STEP_EVERY, Schedule,
    SessionSeen, pins, step,
};
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
        self.worktree_at(&wt, &format!("agent/{name}"));
        wt
    }

    /// A linked worktree where a person makes one, `aterm-wt-<name>` beside the checkout, on a
    /// new branch `wt/<name>` at the checkout's HEAD, its submodule populated.
    fn person_wt(&self, name: &str) -> PathBuf {
        let wt = self.home.join(format!("aterm-wt-{name}"));
        self.worktree_at(&wt, &format!("wt/{name}"));
        wt
    }

    /// The linked worktree `wt` on a new branch `branch` at the checkout's HEAD, its submodule
    /// populated.
    fn worktree_at(&self, wt: &Path, branch: &str) {
        git(
            &self.checkout(),
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                branch,
                &wt.to_string_lossy(),
                "HEAD",
            ],
        );
        git(wt, &["submodule", "update", "-q", "--init"]);
        isolate(&wt.join("vendor").join("astream"));
    }

    /// The shared build cache's build directory for the workspace `ws`, laid as a build leaves
    /// one: cargo's lock file and one artifact of 64 KiB.
    fn leaf(&self, ws: &Path) -> PathBuf {
        let root = atpkg::build_cache::cache_root(&self.home);
        let leaf = atpkg::build_cache::build_dir(&root, ws);
        let deps = leaf.join("debug").join("deps");
        std::fs::create_dir_all(&deps).expect("a build folder");
        std::fs::write(leaf.join("debug").join(".cargo-lock"), "").expect("written");
        std::fs::write(deps.join("libx.rlib"), vec![0_u8; 64 * 1024]).expect("written");
        leaf
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

/// Keep the repository `dir` from this machine's own git settings that change what git reads in
/// a tree (a global ignore file or attributes, line-ending conversion): every git run in it,
/// the step's own included, reads its local config over the global one.
#[track_caller]
fn isolate(dir: &Path) {
    for (key, value) in [
        ("core.excludesFile", "/dev/null"),
        ("core.attributesFile", "/dev/null"),
        ("core.autocrlf", "false"),
    ] {
        git(dir, &["config", key, value]);
    }
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
        isolate(repo);
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
    isolate(&checkout.join("vendor").join("astream"));
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
    /// The sessions the window holds, `None`: not known.
    roster: Option<Vec<SessionSeen>>,
    /// The build-start record's lines.
    starts: Vec<atpkg::build_start::Start>,
    /// The file of owed pushes, `None`: the pinned rule is off.
    owed: Option<PathBuf>,
    /// The step's now, where it is not the scratch's.
    now: Option<SystemTime>,
    /// A worktree whose use by a process cannot be ruled out, as the step's own question of it
    /// is answered.
    using: Option<PathBuf>,
    /// A worktree a person edits between the step's judgment and its question whether a
    /// process uses it: asked, the stub writes `late.rs` in it.
    edits_late: Option<PathBuf>,
    /// A worktree a build starts in after the step's look read the build-start record: the
    /// record read again holds a line of it dated the step's now.
    builds_late: Option<PathBuf>,
    /// A worktree a process comes to use after the step first asked whether one does: the first
    /// question of it is answered clear, every later one is not.
    using_late: Option<PathBuf>,
    /// The pass lock is lost as the first build directory's intent goes on record. The step may
    /// then stop short, which [`run`] lets it.
    lock_lost_at_intent: bool,
    /// Why the custody writes wait, as the home volume reads under its emergency line; `None`:
    /// they run.
    custody: Option<String>,
    /// What the step put on record of each worktree, carried from one run to the next as a
    /// host's schedule carries it.
    said: RefCell<BTreeMap<PathBuf, String>>,
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
    let now = setup.now.unwrap_or(s.now);
    let facts = Facts {
        home: &s.home,
        transcripts_root: Some(&transcripts),
        cwds: &cwds,
        cwd_unknown: setup.cwd_unknown.as_deref(),
        open: &setup.open,
        table: None,
        known: &known,
        roster: setup.roster.as_deref(),
        starts: &setup.starts,
        owed: setup.owed.as_deref(),
        build_dirs: Some(UNWRITTEN),
        build_rested: &[],
    };
    let rows = RefCell::new(Vec::new());
    let lost = Cell::new(false);
    let asked_late = Cell::new(0_u32);
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
        retired::remove_worktree_with_processes(&GitCli, &s.home, checkout, wt, now, &probe)
    };
    let mut remove_pinned = |checkout: &Path, wt: &Path, pinned: &retired::Pinned| {
        retired::remove_pinned_worktree_with_processes(
            &GitCli, &s.home, checkout, wt, now, pinned, &probe,
        )
    };
    let mut journal = |row: &str| {
        if setup.journal_off {
            return false;
        }
        rows.borrow_mut().push(row.to_owned());
        if setup.lock_lost_at_intent && row.contains(crate::harness::disk::SWEEP_ROW_KIND) {
            lost.set(true);
        }
        true
    };
    let still_held = || !lost.get();
    let unused = |wt: &Path| -> Result<(), String> {
        if setup.using.as_deref() == Some(wt) {
            return Err("active process use could not be ruled out".to_owned());
        }
        if setup.using_late.as_deref() == Some(wt) {
            asked_late.set(asked_late.get() + 1);
            if asked_late.get() > 1 {
                return Err("a process came to use it".to_owned());
            }
        }
        if setup.edits_late.as_deref() == Some(wt) {
            std::fs::write(wt.join("late.rs"), "\n").expect("written");
        }
        Ok(())
    };
    let free = |_: &Path| Some(u64::MAX);
    let agh = |_: &[&str]| -> Result<std::process::Output, String> {
        Err("no agh runs in a test".to_owned())
    };
    let starts = || {
        let mut lines = setup.starts.clone();
        if let Some(wt) = &setup.builds_late {
            lines.push(atpkg::build_start::Start {
                at_ms: unix_ms(now),
                ..start(&sid(9), wt)
            });
        }
        lines
    };
    let mut said = setup.said.borrow_mut();
    let custody = || setup.custody.clone().map_or(Ok(()), Err);
    // Judging may take half the wall: the step's own split leaves each phase a one-minute hang
    // detector.
    let (done, _) = step(
        &GitCli,
        &facts,
        &found,
        None,
        now,
        Instant::now() + MUST_HAPPEN * 2,
        Hooks {
            still_held: &still_held,
            remove: &mut remove,
            remove_pinned: &mut remove_pinned,
            journal: &mut journal,
            unused: &unused,
            free: &free,
            agh: &agh,
            said: &mut *said,
            remove_build: &mut |wt, leave, record| {
                atpkg::build_cache::reclaim_merged_worktree_with_processes(
                    &s.home, wt, now, leave, record, &probe,
                )
            },
            starts: &starts,
            custody: &custody,
        },
    );
    if !setup.lock_lost_at_intent {
        assert_eq!(done.unfinished, None, "the step ran through: {done:#?}");
    }
    (done, rows.into_inner())
}

/// `at` in milliseconds since the Unix epoch, as a build-start line dates itself.
fn unix_ms(at: SystemTime) -> u64 {
    let epoch = SystemTime::UNIX_EPOCH;
    let since = at.duration_since(epoch).expect("after the epoch");
    u64::try_from(since.as_millis()).expect("in range")
}

/// What a host's own step ([`crate::harness::cli::disk_worktree_step`]) reads in the scratch:
/// its state folder, its transcripts root and its now.
struct Host {
    state: PathBuf,
    transcripts: PathBuf,
    now: i64,
    /// The home volume's emergency line in bytes: `u64::MAX` reads every volume under it.
    emergency: u64,
}

impl Host {
    fn of(s: &Scratch) -> Self {
        Self {
            state: s.home.join("state"),
            transcripts: s.transcripts(),
            now: i64::try_from(unix_ms(s.now) / 1000).expect("in range"),
            emergency: 0,
        }
    }

    /// The look a host hands its step: no session, no build-start line, no record to read
    /// again.
    fn look<'a>(&'a self, s: &'a Scratch) -> crate::harness::cli::WorktreeLook<'a> {
        crate::harness::cli::WorktreeLook {
            state: &self.state,
            home: &s.home,
            now: self.now,
            transcripts: Some(&self.transcripts),
            session_cwds: &[],
            session_cwd_unknown: None,
            open: &[],
            table: None,
            roster: None,
            starts: &[],
            record: None,
            lines: aterm_capacity::floor::Lines {
                warn: 0,
                floor: 0,
                target: 0,
                emergency: self.emergency,
                files_emergency: 0,
            },
        }
    }

    /// One step of the host's own entry under `config` and `schedule`, every process question
    /// answered by an inert probe and never by this machine's process table.
    fn step(
        &self,
        s: &Scratch,
        config: crate::harness::disk::Config,
        schedule: &mut Schedule,
    ) -> Done {
        let clear = |_: &[PathBuf], _: &[u32]| Ok(PathScan::default());
        crate::harness::cli::disk_worktree_step_with_processes(
            &self.look(s),
            config,
            schedule,
            &|| true,
            &clear,
        )
    }

    /// The ledger's rows, none where it was never written.
    fn ledger(&self) -> Vec<String> {
        std::fs::read_to_string(self.state.join(crate::harness::cli::DISK_LEDGER))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

/// The entries under a folder by their paths from it: a folder as `None`, a file as its bytes
/// and its modification time, a link as its target and its own time.
type Entries = BTreeMap<PathBuf, Option<(Vec<u8>, SystemTime)>>;

/// Every entry under `dir` ([`Entries`]).
fn entries_of(dir: &Path) -> Entries {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(folder) = stack.pop() {
        for entry in std::fs::read_dir(&folder).expect("a folder read") {
            let path = entry.expect("an entry").path();
            let meta = std::fs::symlink_metadata(&path).expect("its metadata");
            let rel = path.strip_prefix(dir).expect("under it").to_path_buf();
            if meta.is_dir() {
                out.insert(rel, None);
                stack.push(path);
                continue;
            }
            let bytes = if meta.file_type().is_symlink() {
                std::fs::read_link(&path)
                    .expect("its target")
                    .into_os_string()
                    .into_encoded_bytes()
            } else {
                std::fs::read(&path).expect("its bytes")
            };
            out.insert(rel, Some((bytes, meta.modified().expect("its time"))));
        }
    }
    out
}

/// All of the worktree `wt` that a step must leave as it is: every entry of its tree and every
/// entry of git's own folder for it, the store of its submodule included.
fn standing(wt: &Path) -> [Entries; 2] {
    [entries_of(wt), entries_of(&git_entry(wt))]
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

/// The rows of kind `build-cache-sweep`, parsed.
fn sweep_rows(rows: &[String]) -> Vec<Value> {
    rows.iter()
        .map(|r| aterm_json::from_str::<Value>(r).expect("a JSON row"))
        .filter(|v| v["kind"].as_str() == Some(crate::harness::disk::SWEEP_ROW_KIND))
        .collect()
}

/// The actions of the `build-cache-sweep` rows that name `path`, in the order they went on
/// record.
fn sweeps_of(rows: &[String], path: &Path) -> Vec<String> {
    let path = path.to_string_lossy();
    sweep_rows(rows)
        .iter()
        .filter(|v| v["path"].as_str() == Some(&*path))
        .filter_map(|v| v["action"].as_str().map(str::to_owned))
        .collect()
}

/// Git's own folder for the linked worktree `wt` (`<checkout>/.git/worktrees/<name>`), as its
/// `.git` file names it.
fn git_entry(wt: &Path) -> PathBuf {
    let entry = std::fs::read_to_string(wt.join(".git")).expect("its .git");
    PathBuf::from(
        entry
            .trim()
            .strip_prefix("gitdir:")
            .expect("a gitfile")
            .trim(),
    )
}

/// The bytes and the modification time of each file of git's own for `wt` that a checkout, a
/// reset or an index refresh writes: its `index`, `HEAD` and `logs/HEAD`.
fn git_files(wt: &Path) -> Vec<(Vec<u8>, SystemTime)> {
    let entry = git_entry(wt);
    ["index", "HEAD", "logs/HEAD"]
        .iter()
        .map(|name| {
            let file = entry.join(name);
            let meta = std::fs::metadata(&file).expect("its metadata");
            let bytes = std::fs::read(&file).expect("its bytes");
            (bytes, meta.modified().expect("its time"))
        })
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
    let host = Host::of(&s);
    let mut schedule = Schedule::new();
    let done = crate::harness::cli::disk_worktree_step(
        &host.look(&s),
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
        host.state
            .join(crate::harness::cli::DISK_LEDGER)
            .symlink_metadata()
            .is_err(),
        "no ledger row"
    );
    assert!(schedule.due(Instant::now(), || false), "the step stays due");
}

/// [`scratch`] with origins: the submodule's commit cloned into a bare `astream-origin.git`, which
/// the checkout's submodule (and so its module witness) names as its origin, and the checkout's
/// `main` pushed to a bare `origin.git`, its `origin`, so `origin/main` is what a push set.
fn scratch_with_origin() -> Scratch {
    let dir = aterm_tempfile::Builder::new()
        .prefix("disk-pins-")
        .suffix(".noindex")
        .tempdir()
        .expect("scratch");
    let home = std::fs::canonicalize(dir.path())
        .expect("resolved")
        .join("home");
    let sub = home.join("astream");
    let sub_origin = home.join("astream-origin.git");
    let checkout = home.join("aterm");
    let origin = home.join("origin.git");
    std::fs::create_dir_all(&sub).expect("a repo");
    git(&sub, &["init", "-q"]);
    isolate(&sub);
    std::fs::write(sub.join("lib.rs"), "// sub\n").expect("written");
    git(&sub, &["add", "lib.rs"]);
    git(&sub, &["commit", "-qm", "sub"]);
    git(
        &home,
        &[
            "clone",
            "-q",
            "--bare",
            &sub.to_string_lossy(),
            &sub_origin.to_string_lossy(),
        ],
    );
    std::fs::create_dir_all(&checkout).expect("a repo");
    git(&checkout, &["init", "-q"]);
    isolate(&checkout);
    std::fs::write(checkout.join("Cargo.toml"), "[workspace]\n").expect("written");
    std::fs::write(checkout.join(".gitignore"), "/target*\n/.claude/\n").expect("written");
    git(&checkout, &["add", "Cargo.toml", ".gitignore"]);
    git(
        &checkout,
        &[
            "submodule",
            "add",
            "-q",
            &sub_origin.to_string_lossy(),
            "vendor/astream",
        ],
    );
    isolate(&checkout.join("vendor").join("astream"));
    git(&checkout, &["commit", "-qm", "main"]);
    git(&home, &["init", "-q", "--bare", &origin.to_string_lossy()]);
    git(
        &checkout,
        &["remote", "add", "origin", &origin.to_string_lossy()],
    );
    git(&checkout, &["push", "-q", "origin", "main"]);
    Scratch {
        _dir: dir,
        home,
        now: SystemTime::now() + UNWRITTEN + DAY,
    }
}

/// The checkout's origin.
fn origin(s: &Scratch) -> PathBuf {
    s.home.join("origin.git")
}

/// The file of owed pushes in the scratch's state folder.
fn owed_file(s: &Scratch) -> PathBuf {
    s.home.join("state").join(pins::OWED_FILE)
}

/// `git <args>` in `dir`, which must succeed: its output, trimmed.
#[track_caller]
fn out(dir: &Path, args: &[&str]) -> String {
    let a = GitCli.run(dir, args);
    assert_eq!(
        a.code,
        Some(0),
        "git {args:?} in {}: {}",
        dir.display(),
        a.err
    );
    a.out.trim().to_owned()
}

/// The refs of the repository `dir` under `prefix`, each `(name, commit)`.
fn refs(dir: &Path, prefix: &str) -> Vec<(String, String)> {
    out(
        dir,
        &["for-each-ref", "--format=%(refname) %(objectname)", prefix],
    )
    .lines()
    .filter_map(|l| l.split_once(' '))
    .map(|(n, o)| (n.to_owned(), o.to_owned()))
    .collect()
}

/// The refs the pins of the worktree `aterm-<name>.noindex` hold in the repository `dir`.
fn pins_of(dir: &Path, name: &str) -> Vec<(String, String)> {
    let mut all = Vec::new();
    for kind in ["refs/snapshots", "refs/orphans", "refs/snapshots-local"] {
        all.extend(refs(dir, &format!("{kind}/aterm-{name}/")));
    }
    all
}

/// Whether the commit `oid` is in the history of the ref `name` of the repository `dir`.
fn holds(dir: &Path, name: &str, oid: &str) -> bool {
    GitCli
        .run(dir, &["merge-base", "--is-ancestor", oid, name])
        .code
        == Some(0)
}

/// A session id the build-start record takes.
fn sid(n: u32) -> String {
    format!("s-{n:020x}")
}

/// A build-start line of the session `session` for the workspace `wt`.
fn start(session: &str, wt: &Path) -> atpkg::build_start::Start {
    atpkg::build_start::Start {
        session: session.to_owned(),
        workspace: wt.to_path_buf(),
        build: None,
        pid: 7,
        at_ms: 0,
    }
}

/// ONE WORKTREE OF EACH KIND ENDS WITH NONE, AND EVERY COMMIT AND EDIT ON A REF: of seven agents'
/// worktrees whose sessions are gone (no build-start line, nothing written for a week), the
/// merged, clean one goes by the merged rule with its branch; one with a commit `origin/main`
/// lacks, one detached at such a commit, one with an edit, one with an untracked file, one whose
/// index differs from both its HEAD and its files, and one whose only new commit its reflog
/// holds each go once that is on a ref of the checkout that its origin holds too: the commit on
/// `refs/orphans/…`, the edit, the file and the reflog's commit on a snapshot whose tree and
/// parents hold them, the staged text on the snapshot's second parent; the pinned ones keep
/// their branches and no push is owed. NEGATIVE CONTROLS: with no file to record owed pushes
/// (the pinned rule off) the six stay (the DC-02a test); a snapshot that dropped the reflog's
/// tip or the index parent fails the `holds` and `show` reads.
#[test]
fn one_worktree_of_each_kind_ends_with_none_and_every_commit_and_edit_on_a_ref() {
    let s = scratch_with_origin();
    let merged = s.wt("merged");
    let ahead = s.wt("ahead");
    std::fs::write(ahead.join("ahead.rs"), "// ahead\n").expect("written");
    git(&ahead, &["add", "ahead.rs"]);
    git(&ahead, &["commit", "-qm", "ahead"]);
    let ahead_head = out(&ahead, &["rev-parse", "HEAD"]);
    let detached = s.wt("detached");
    git(&detached, &["checkout", "-q", "--detach"]);
    std::fs::write(detached.join("detached.rs"), "// detached\n").expect("written");
    git(&detached, &["add", "detached.rs"]);
    git(&detached, &["commit", "-qm", "detached"]);
    let detached_head = out(&detached, &["rev-parse", "HEAD"]);
    let edited = s.wt("edited");
    std::fs::write(edited.join("Cargo.toml"), "[workspace] # edited\n").expect("edited");
    let untracked = s.wt("untracked");
    std::fs::write(untracked.join("notes.txt"), "notes\n").expect("written");
    let staged = s.wt("staged");
    std::fs::write(staged.join("Cargo.toml"), "[workspace] # staged\n").expect("written");
    git(&staged, &["add", "Cargo.toml"]);
    std::fs::write(staged.join("Cargo.toml"), "[workspace] # on disk\n").expect("written");
    let reflog = s.wt("reflog");
    std::fs::write(reflog.join("lost.rs"), "// lost\n").expect("written");
    git(&reflog, &["add", "lost.rs"]);
    git(&reflog, &["commit", "-qm", "lost"]);
    let lost = out(&reflog, &["rev-parse", "HEAD"]);
    git(&reflog, &["reset", "-q", "--hard", "HEAD~1"]);
    let (done, rows) = run(
        &s,
        &Setup {
            owed: Some(owed_file(&s)),
            ..Setup::default()
        },
    );
    for wt in [
        &merged, &ahead, &detached, &edited, &untracked, &staged, &reflog,
    ] {
        assert!(went(&done, wt), "{} went: {done:#?}", wt.display());
    }
    assert!(
        done.owed.is_empty() && done.unrecorded.is_none(),
        "{done:#?}"
    );
    let removed = |wt: &Path| done.removed.iter().find(|r| r.path == wt).expect("removed");
    assert_eq!(removed(&merged).branch_deleted, Ok(()));
    assert_eq!(removed(&merged).pins, None);
    assert_eq!(
        s.rev(&s.checkout(), "refs/heads/agent/ahead"),
        Some(ahead_head.clone())
    );
    let origin = origin(&s);
    let pinned = |name: &str| -> String {
        let local = pins_of(&s.checkout(), name);
        assert_eq!(local.len(), 1, "one pin of {name}: {local:?}");
        assert_eq!(
            pins_of(&origin, name),
            local,
            "its origin holds the pin of {name}"
        );
        local[0].0.clone()
    };
    let at = pinned("ahead");
    assert!(
        at.starts_with("refs/orphans/") && holds(&origin, &at, &ahead_head),
        "{at}"
    );
    let at = pinned("detached");
    assert!(at.starts_with("refs/orphans/") && holds(&origin, &at, &detached_head));
    let at = pinned("edited");
    assert_eq!(
        out(&origin, &["show", &format!("{at}:Cargo.toml")]),
        "[workspace] # edited"
    );
    let at = pinned("untracked");
    assert_eq!(out(&origin, &["show", &format!("{at}:notes.txt")]), "notes");
    let at = pinned("staged");
    assert_eq!(
        out(&origin, &["show", &format!("{at}:Cargo.toml")]),
        "[workspace] # on disk"
    );
    assert_eq!(
        out(&origin, &["show", &format!("{at}^2:Cargo.toml")]),
        "[workspace] # staged"
    );
    let at = pinned("reflog");
    assert!(
        at.starts_with("refs/snapshots/") && holds(&origin, &at, &lost),
        "{at}"
    );
    let removed_rows = worktree_rows(&rows)
        .into_iter()
        .filter(|r| r["action"].as_str() == Some("removed"))
        .count();
    assert_eq!(removed_rows, 7, "each removal is on record");
}

/// A SNAPSHOT WRITES NEITHER THE TREE NOR ITS INDEX: pinning a worktree with an edit, a staged
/// change and an untracked file leaves every file and folder of it, and its index, byte for
/// byte and time for time as they were, while the snapshot it writes holds all three, its tree
/// the one the removal holds the worktree to, and no private index is left in the checkout's
/// git directory. NEGATIVE CONTROLS: a snapshot read through the worktree's own index (no
/// `GIT_INDEX_FILE`) stages the untracked file there, and the index's bytes differ; one that
/// gave the remover HEAD's tree fails the `rev-parse` read.
#[test]
fn a_snapshot_writes_neither_the_tree_nor_its_index() {
    use std::time::UNIX_EPOCH;
    let s = scratch_with_origin();
    let wt = s.wt("quiet");
    std::fs::write(wt.join("Cargo.toml"), "[workspace] # staged\n").expect("written");
    git(&wt, &["add", "Cargo.toml"]);
    std::fs::write(wt.join("Cargo.toml"), "[workspace] # on disk\n").expect("written");
    std::fs::write(wt.join("notes.txt"), "notes\n").expect("written");
    let entry = PathBuf::from(out(
        &wt,
        &["rev-parse", "--path-format=absolute", "--git-dir"],
    ));
    let seen = |root: &Path| -> Vec<(PathBuf, Vec<u8>, u128)> {
        let mut all = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(at) = stack.pop() {
            let meta = std::fs::symlink_metadata(&at).expect("readable");
            let when = meta
                .modified()
                .expect("dated")
                .duration_since(UNIX_EPOCH)
                .expect("after the epoch")
                .as_nanos();
            if meta.is_dir() {
                for e in std::fs::read_dir(&at).expect("listed") {
                    stack.push(e.expect("an entry").path());
                }
                all.push((at, Vec::new(), when));
            } else {
                let bytes = std::fs::read(&at).expect("read");
                all.push((at, bytes, when));
            }
        }
        all.sort();
        all
    };
    let tree = seen(&wt);
    let index = seen(&entry.join("index"));
    let admitted =
        retired::judge_pinned_worktree(&GitCli, &s.home, &s.checkout(), &wt, s.now, UNWRITTEN)
            .expect("admitted");
    let pinned = pins::pin(
        &GitCli,
        &s.checkout(),
        &wt,
        &admitted,
        UNWRITTEN,
        Some(u64::MAX),
        pins::STEP_BYTES,
    )
    .expect("pinned");
    assert_eq!(seen(&wt), tree, "the tree is as it was");
    assert_eq!(seen(&entry.join("index")), index, "its index is as it was");
    let (at, _) = pinned.pinned.roots.first().expect("a snapshot ref").clone();
    let checkout = s.checkout();
    assert_eq!(
        out(&checkout, &["rev-parse", &format!("{at}^{{tree}}")]),
        pinned.pinned.tree,
        "the tree its removal holds it to is the snapshot's"
    );
    assert_eq!(
        out(&checkout, &["show", &format!("{at}:Cargo.toml")]),
        "[workspace] # on disk"
    );
    assert_eq!(
        out(&checkout, &["show", &format!("{at}:notes.txt")]),
        "notes"
    );
    assert_eq!(
        out(&checkout, &["show", &format!("{at}^2:Cargo.toml")]),
        "[workspace] # staged"
    );
    let stray: Vec<_> = std::fs::read_dir(checkout.join(".git"))
        .expect("listed")
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("aterm-pin-"))
        .collect();
    assert!(stray.is_empty(), "its private index went: {stray:?}");
}

/// A FAILED PUSH STILL REMOVES THE PINNED WORKTREE, AND IS OWED: with its origin unreachable a
/// worktree holding a commit `origin/main` lacks goes once that commit is on a ref of its
/// checkout, and the push is in the owed file with why it failed, its worktree marked gone and
/// its next try an hour on; a round before then tries nothing, and the round after it, the
/// origin back, pushes it, reads it back there and owes nothing. NEGATIVE CONTROLS: a step that
/// kept a worktree for a failed push fails `went`; one that recorded nothing owed pushes nothing
/// in the next round; one with no backoff pushes in the early round.
#[test]
fn a_failed_push_still_removes_the_pinned_worktree_and_is_owed() {
    let s = scratch_with_origin();
    let wt = s.wt("offline");
    std::fs::write(wt.join("work.rs"), "// work\n").expect("written");
    git(&wt, &["add", "work.rs"]);
    git(&wt, &["commit", "-qm", "work"]);
    let head = out(&wt, &["rev-parse", "HEAD"]);
    let gone = s.home.join("gone.git");
    git(
        &s.checkout(),
        &["remote", "set-url", "origin", &gone.to_string_lossy()],
    );
    let (done, _) = run(
        &s,
        &Setup {
            owed: Some(owed_file(&s)),
            ..Setup::default()
        },
    );
    assert!(went(&done, &wt), "{done:#?}");
    let pin = pins_of(&s.checkout(), "offline");
    assert_eq!(pin.len(), 1, "{pin:?}");
    assert!(holds(&s.checkout(), &pin[0].0, &head));
    assert_eq!(done.pushed, 0);
    assert_eq!(done.owed.len(), 1, "{done:#?}");
    assert!(
        !done.owed[0].why.is_empty(),
        "why it is owed: {:?}",
        done.owed[0]
    );
    let recorded = pins::load(&owed_file(&s)).expect("the owed file");
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    assert_eq!(
        recorded[0].push.refs,
        vec![[pin[0].0.clone(), pin[0].1.clone()]]
    );
    assert!(pins_of(&origin(&s), "offline").is_empty());
    git(
        &s.checkout(),
        &["remote", "set-url", "origin", &origin(&s).to_string_lossy()],
    );
    assert!(
        recorded[0].removed && recorded[0].tries == 1,
        "{recorded:?}"
    );
    let no_agh = |_: &[&str]| -> Result<std::process::Output, String> {
        Err("no agh runs in a test".to_owned())
    };
    let at = pins::epoch_secs(s.now);
    let until = Instant::now() + MUST_HAPPEN;
    let early = pins::push_owed(&GitCli, &no_agh, &owed_file(&s), at, until);
    assert_eq!(
        (early.pushed.len(), early.owed.len()),
        (0, 1),
        "not due yet: {early:#?}"
    );
    assert!(pins_of(&origin(&s), "offline").is_empty());
    let later = at + 2 * 60 * 60;
    let round = pins::push_owed(&GitCli, &no_agh, &owed_file(&s), later, until);
    assert_eq!((round.pushed.len(), round.owed.len()), (1, 0), "{round:#?}");
    assert!(round.unrecorded.is_none());
    assert_eq!(pins_of(&origin(&s), "offline"), pin);
    assert!(
        pins::load(&owed_file(&s))
            .expect("the owed file")
            .is_empty()
    );
}

/// A GONE SESSION'S WORKTREE WAITS BELOW THE EMERGENCY LINE, AND GOES ABOVE IT (DC-06d): while
/// the home volume reads under its emergency line, a worktree whose session is gone with an edit
/// and an untracked file stays across two steps, its tree and git's own files for it as they
/// were, with no ref of it in its checkout or at its origin, no owed file written, and one
/// `deferred` row on record for both steps; the merged, clean control goes in the first, since
/// its removal pins nothing. The first step once the volume reads above the line pins it,
/// removes it and pushes its snapshot, which holds the edit and the file at its origin.
/// NEGATIVE CONTROLS: a step that pinned below the line fails the empty `pins_of`; one that
/// journalled each step's deferral fails the single row; one that never took the worktree up
/// again fails `went`.
#[test]
fn a_gone_sessions_worktree_waits_below_the_emergency_line_and_goes_above_it() {
    let s = scratch_with_origin();
    let merged = s.wt("merged");
    let orphan = s.wt("orphan");
    std::fs::write(orphan.join("Cargo.toml"), "[workspace] # edited\n").expect("edited");
    std::fs::write(orphan.join("notes.txt"), "notes\n").expect("written");
    let as_was = standing(&orphan);
    let mut setup = Setup {
        owed: Some(owed_file(&s)),
        custody: Some("its volume reads under its emergency line".to_owned()),
        ..Setup::default()
    };
    let mut rows = Vec::new();
    for at in 0..2 {
        let (done, more) = run(&s, &setup);
        rows.extend(more);
        if at == 0 {
            assert!(went(&done, &merged), "the merged control goes: {done:#?}");
        }
        assert!(!went(&done, &orphan), "it waits: {done:#?}");
        assert_eq!(
            done.deferred.iter().map(|(p, _)| p).collect::<Vec<_>>(),
            vec![&orphan],
            "{done:#?}"
        );
        assert!(done.waits() && done.removed.iter().all(|r| r.pins.is_none()));
        assert_eq!(standing(&orphan), as_was, "nothing of it was written");
        assert!(
            pins_of(&s.checkout(), "orphan").is_empty(),
            "no pin below the line"
        );
        assert!(
            pins_of(&origin(&s), "orphan").is_empty(),
            "no push below the line"
        );
        assert!(
            owed_file(&s).symlink_metadata().is_err(),
            "no owed file below the line"
        );
    }
    let deferred: Vec<Value> = worktree_rows(&rows)
        .into_iter()
        .filter(|r| r["action"].as_str() == Some(crate::harness::disk::WORKTREE_DEFERRED))
        .collect();
    assert_eq!(
        deferred.len(),
        1,
        "one deferred row for both steps: {rows:#?}"
    );
    assert_eq!(
        deferred[0]["path"].as_str(),
        Some(&*orphan.to_string_lossy())
    );
    assert_eq!(
        deferred[0]["text"].as_str(),
        Some("its volume reads under its emergency line")
    );
    setup.custody = None;
    let (done, _) = run(&s, &setup);
    assert!(went(&done, &orphan), "taken up above the line: {done:#?}");
    assert!(!done.waits(), "{done:#?}");
    let pin = pins_of(&origin(&s), "orphan");
    assert_eq!(pin.len(), 1, "its snapshot is at its origin: {pin:?}");
    assert_eq!(pin, pins_of(&s.checkout(), "orphan"));
    let at = &pin[0].0;
    assert_eq!(
        out(&origin(&s), &["show", &format!("{at}:Cargo.toml")]),
        "[workspace] # edited"
    );
    assert_eq!(
        out(&origin(&s), &["show", &format!("{at}:notes.txt")]),
        "notes"
    );
}

/// A SECOND WAIT IS RECORDED AGAIN (DC-06d): a gone session's worktree deferred below the
/// emergency line, with its `deferred` row on record, is resumed by a session before a step that
/// reads the volume above the line reaches it, so that step keeps it as live; once the session is
/// gone again and the volume reads under the line again, the step defers it with a second
/// `deferred` row, since the step above the line ended the first wait. A step under the line
/// after that writes no third. NEGATIVE CONTROL: a step above the line that left the first wait
/// on record (cleared only where its worktree goes) writes no second row.
#[test]
fn a_second_wait_after_the_volume_read_above_the_line_is_recorded_again() {
    let s = scratch_with_origin();
    let orphan = s.wt("orphan");
    std::fs::write(orphan.join("notes.txt"), "notes\n").expect("written");
    let under = Some("its volume reads under its emergency line".to_owned());
    let deferred_rows = |rows: &[String]| {
        worktree_rows(rows)
            .into_iter()
            .filter(|r| {
                r["action"].as_str() == Some(crate::harness::disk::WORKTREE_DEFERRED)
                    && r["path"].as_str() == Some(&*orphan.to_string_lossy())
            })
            .count()
    };
    let mut setup = Setup {
        owed: Some(owed_file(&s)),
        custody: under.clone(),
        ..Setup::default()
    };
    let (done, rows) = run(&s, &setup);
    assert_eq!(done.deferred.len(), 1, "{done:#?}");
    assert_eq!(deferred_rows(&rows), 1, "{rows:#?}");

    setup.custody = None;
    setup.cwds = vec![orphan.clone()];
    let (done, rows) = run(&s, &setup);
    assert!(
        !went(&done, &orphan) && done.deferred.is_empty(),
        "{done:#?}"
    );
    assert_eq!(deferred_rows(&rows), 0, "{rows:#?}");

    setup.custody = under;
    setup.cwds = Vec::new();
    let (done, rows) = run(&s, &setup);
    assert_eq!(done.deferred.len(), 1, "{done:#?}");
    assert_eq!(
        deferred_rows(&rows),
        1,
        "the second wait is recorded: {rows:#?}"
    );
    let (_, rows) = run(&s, &setup);
    assert_eq!(deferred_rows(&rows), 0, "once for each wait: {rows:#?}");
    assert!(orphan.is_dir() && pins_of(&s.checkout(), "orphan").is_empty());
}

/// THE SCHEDULE TAKES WAITING WRITES UP AT THE FIRST LOOK ABOVE THE LINE (DC-06d): a run that
/// left writes waiting is due before its hour only where the look reads the volume above the
/// line, and a run that the wait made due while a fresh reading is under the line stands down;
/// one that left none is due only at its hour, and the volume is not asked of it. A run that
/// stopped short keeps the wait the run before it left, since it may not have reached those
/// writes; a run that went through, or that stopped short with no wait before it, leaves none.
/// NEGATIVE CONTROLS: a `due` without the wait's clause fails the early due; a `stands_down`
/// that ignored the hour stands down at it; a `ran` that set the wait from its own run alone
/// fails the kept wait.
#[test]
fn the_schedule_takes_waiting_writes_up_at_the_first_look_above_the_line() {
    let t0 = Instant::now();
    let soon = t0 + Duration::from_secs(60);
    let hour = t0 + STEP_EVERY;
    let unasked = || -> bool { panic!("the volume is not asked") };

    let mut fresh = Schedule::new();
    assert!(fresh.due(t0, unasked) && !fresh.stands_down(t0, unasked));
    fresh.ran(t0, None, false);
    assert!(!fresh.due(soon, unasked), "no wait: due at its hour only");
    assert!(fresh.due(hour, unasked));
    assert!(!fresh.stands_down(soon, unasked));

    let mut waits = Schedule::new();
    waits.ran(t0, None, true);
    assert!(!waits.due(soon, || false), "under the line it waits");
    assert!(waits.due(soon, || true), "the first look above the line");
    assert!(waits.due(hour, unasked), "its hour needs no reading");
    assert!(
        waits.stands_down(soon, || true),
        "a fresh reading under the line"
    );
    assert!(
        !waits.stands_down(soon, || false),
        "a fresh reading above it"
    );
    assert!(
        !waits.stands_down(hour, unasked),
        "its hour runs whatever the volume reads"
    );

    let reached = Some(PathBuf::from("/wt/b"));
    waits.ran(t0, reached.clone(), false);
    assert!(waits.due(soon, || true), "stopped short, the wait stands");
    waits.ran(t0, None, false);
    assert!(!waits.due(soon, unasked), "went through: no wait");
    waits.ran(t0, reached, false);
    assert!(
        !waits.due(soon, unasked),
        "stopped short with no wait before"
    );
}

/// A RUN THE WAIT MADE DUE STANDS DOWN UNDER THE LINE (DC-06d): the host's own step, its
/// schedule left waiting by a run a minute ago and its emergency line above anything the volume
/// can read, does nothing: a merged, clean worktree stays and the ledger gets no row. The same
/// step with the line at zero removes it. NEGATIVE CONTROL: without the stand-down the first
/// step removes the merged worktree, whose removal pins nothing.
#[test]
fn a_run_the_wait_made_due_stands_down_under_the_line() {
    use crate::harness::disk::Config;

    let s = scratch();
    let c = s.wt("c");
    let mut host = Host::of(&s);
    let waiting = || {
        let mut schedule = Schedule::new();
        schedule.ran(Instant::now(), None, true);
        schedule
    };
    host.emergency = u64::MAX;
    let done = host.step(&s, Config::default(), &mut waiting());
    assert!(done.removed.is_empty() && done.kept.is_empty(), "{done:#?}");
    assert!(s.listed(&c), "it stays");
    assert!(host.ledger().is_empty(), "no row: {:#?}", host.ledger());

    host.emergency = 0;
    let done = host.step(&s, Config::default(), &mut waiting());
    assert!(went(&done, &c), "{done:#?}");
}

/// OWED PUSHES WAIT BELOW THE EMERGENCY LINE (DC-06d): a push owed since its origin was down,
/// due again with the origin back, is not tried while the home volume reads under its emergency
/// line: two steps there push nothing, say why it waits, and put one `deferred` push row on
/// record; the first step above the line pushes it. NEGATIVE CONTROLS: a round that pushed below
/// the line fails the empty `pins_of` at origin; one that journalled each step's wait fails the
/// single row.
#[test]
fn owed_pushes_wait_below_the_emergency_line() {
    let s = scratch_with_origin();
    let wt = s.wt("offline");
    std::fs::write(wt.join("work.rs"), "// work\n").expect("written");
    git(&wt, &["add", "work.rs"]);
    git(&wt, &["commit", "-qm", "work"]);
    let gone = s.home.join("gone.git");
    git(
        &s.checkout(),
        &["remote", "set-url", "origin", &gone.to_string_lossy()],
    );
    let mut setup = Setup {
        owed: Some(owed_file(&s)),
        ..Setup::default()
    };
    let (done, _) = run(&s, &setup);
    assert!(went(&done, &wt) && done.owed.len() == 1, "{done:#?}");
    git(
        &s.checkout(),
        &["remote", "set-url", "origin", &origin(&s).to_string_lossy()],
    );
    setup.now = Some(s.now + Duration::from_secs(2 * 60 * 60));
    setup.custody = Some("its volume reads under its emergency line".to_owned());
    let mut rows = Vec::new();
    for _ in 0..2 {
        let (done, more) = run(&s, &setup);
        rows.extend(more);
        assert_eq!(
            (done.pushed, done.owed.len()),
            (0, 1),
            "nothing pushed below the line: {done:#?}"
        );
        assert_eq!(
            done.pushes_deferred.as_deref(),
            Some("its volume reads under its emergency line")
        );
        assert!(pins_of(&origin(&s), "offline").is_empty());
    }
    let waits: Vec<&String> = rows
        .iter()
        .filter(|r| {
            r.contains(crate::harness::disk::WORKTREE_PUSH_ROW_KIND)
                && r.contains("\"action\":\"deferred\"")
        })
        .collect();
    assert_eq!(
        waits.len(),
        1,
        "one deferred push row for both steps: {rows:#?}"
    );
    setup.custody = None;
    let (done, _) = run(&s, &setup);
    assert_eq!((done.pushed, done.owed.len()), (1, 0), "{done:#?}");
    assert!(done.pushes_deferred.is_none());
    assert_eq!(pins_of(&origin(&s), "offline").len(), 1);
}

/// A LIVE SESSION'S WORKTREE IS LEFT ALONE: an hour and ten minutes after the last write, a
/// worktree with an edit whose build-start line names a session the window holds live stays,
/// with nothing of it pinned and no push owed, while the same kind of worktree whose line
/// names a session the window holds exited goes once pinned. NEGATIVE CONTROL: without the
/// live check the first goes as the second does.
#[test]
fn a_live_sessions_worktree_is_left_alone() {
    let s = scratch_with_origin();
    let live = s.wt("live");
    std::fs::write(live.join("Cargo.toml"), "[workspace] # live\n").expect("edited");
    let exited = s.wt("exited");
    std::fs::write(exited.join("Cargo.toml"), "[workspace] # exited\n").expect("edited");
    let now = SystemTime::now() + RECORDS_QUIET + Duration::from_secs(10 * 60);
    let (done, rows) = run(
        &s,
        &Setup {
            owed: Some(owed_file(&s)),
            now: Some(now),
            roster: Some(vec![
                SessionSeen {
                    sid: sid(1),
                    live: true,
                },
                SessionSeen {
                    sid: sid(2),
                    live: false,
                },
            ]),
            starts: vec![start(&sid(1), &live), start(&sid(2), &exited)],
            ..Setup::default()
        },
    );
    let why = kept_for(&done, &live);
    assert!(why.contains("a live session's worktree"), "{why}");
    assert!(live.join("Cargo.toml").is_file() && s.listed(&live));
    assert!(pins_of(&s.checkout(), "live").is_empty());
    assert!(went(&done, &exited), "{done:#?}");
    assert_eq!(pins_of(&s.checkout(), "exited").len(), 1);
    assert!(done.owed.is_empty(), "{done:#?}");
    let named: Vec<Value> = worktree_rows(&rows)
        .into_iter()
        .filter(|r| r["path"].as_str() == Some(&*live.to_string_lossy()))
        .collect();
    assert!(named.is_empty(), "no row names it: {named:?}");
}

/// A SUBMODULE COMMIT NO REMOTE HAS IS PINNED BEFORE ITS WORKTREE GOES: a commit made in a
/// worktree's checked-out submodule lives only in the worktree's own module store; it goes into
/// the checkout's module witness on a ref, which the submodule's origin then holds, and the
/// worktree's snapshot records the submodule at it, before the worktree, and its store, go.
/// NEGATIVE CONTROL: without the copy into the witness the removal's history check keeps the
/// worktree ("is not rooted in the canonical module witness").
#[test]
fn a_submodule_commit_no_remote_has_is_pinned_before_its_worktree_goes() {
    let s = scratch_with_origin();
    let wt = s.wt("sub");
    let module = wt.join("vendor").join("astream");
    std::fs::write(module.join("lib.rs"), "// sub, changed\n").expect("written");
    git(&module, &["commit", "-qam", "sub work"]);
    let sub_head = out(&module, &["rev-parse", "HEAD"]);
    let witness = s.checkout().join(".git/modules/vendor/astream");
    let lacks = GitCli.run(
        &witness,
        &["cat-file", "-e", &format!("{sub_head}^{{commit}}")],
    );
    assert_ne!(lacks.code, Some(0), "only the worktree's store holds it");
    let (done, _) = run(
        &s,
        &Setup {
            owed: Some(owed_file(&s)),
            ..Setup::default()
        },
    );
    assert!(went(&done, &wt), "{done:#?}");
    assert!(done.owed.is_empty(), "{done:#?}");
    let in_witness = pins_of(&witness, "sub");
    assert_eq!(in_witness.len(), 1, "{in_witness:?}");
    assert_eq!(in_witness[0].1, sub_head);
    assert_eq!(
        pins_of(&s.home.join("astream-origin.git"), "sub"),
        in_witness
    );
    let snapshot = pins_of(&s.checkout(), "sub");
    assert_eq!(snapshot.len(), 1, "{snapshot:?}");
    let at = format!("{}:vendor/astream", snapshot[0].0);
    assert_eq!(out(&origin(&s), &["rev-parse", &at]), sub_head);
}

/// IGNORED FILES OUTSIDE BUILD DIRECTORIES STAY ON A REF THAT IS NEVER PUSHED: a worktree whose
/// only work is a file git ignores (`.claude/settings.local.json`) beside a tagged build folder
/// goes once that file is on `refs/snapshots-local/…` of its checkout, whose tree holds it and
/// not the build folder; its origin gets no ref of it and nothing is owed. NEGATIVE CONTROLS: a
/// pin that pushed it fails the origin read; one that took the build folder fails the
/// `ls-tree` read; without the local ref the remover keeps it (no pin holds `.claude/`).
#[test]
fn ignored_files_outside_build_directories_stay_on_a_ref_that_is_never_pushed() {
    let s = scratch_with_origin();
    let wt = s.wt("ignored");
    std::fs::create_dir_all(wt.join(".claude")).expect("a folder");
    std::fs::write(wt.join(".claude/settings.local.json"), "{\"mine\":1}\n").expect("written");
    let target = wt.join("target");
    std::fs::create_dir_all(target.join("debug")).expect("a build folder");
    std::fs::write(
        target.join("CACHEDIR.TAG"),
        "Signature: 8a477f597d28d172789f06886806bc55\n",
    )
    .expect("tagged");
    std::fs::write(target.join("debug").join("app"), "binary\n").expect("written");
    let (done, _) = run(
        &s,
        &Setup {
            owed: Some(owed_file(&s)),
            ..Setup::default()
        },
    );
    assert!(went(&done, &wt), "{done:#?}");
    assert!(done.owed.is_empty() && done.pushed == 0, "{done:#?}");
    let local = pins_of(&s.checkout(), "ignored");
    assert_eq!(local.len(), 1, "{local:?}");
    assert!(local[0].0.starts_with("refs/snapshots-local/"), "{local:?}");
    assert_eq!(
        out(
            &s.checkout(),
            &[
                "show",
                &format!("{}:.claude/settings.local.json", local[0].0)
            ]
        ),
        "{\"mine\":1}"
    );
    let tree = out(&s.checkout(), &["ls-tree", "--name-only", &local[0].0]);
    assert!(!tree.lines().any(|l| l == "target"), "{tree}");
    assert!(pins_of(&origin(&s), "ignored").is_empty());
    assert!(refs(&origin(&s), "refs/snapshots-local/").is_empty());
}

/// A WORKTREE A SESSION THE WINDOW DOES NOT HOLD BUILT IN WAITS SEVEN DAYS: an hour and ten
/// minutes after the last write, a worktree with an edit whose only build-start line names a
/// session the window holds neither live nor exited (another window's, or one from before its
/// start) stays, with nothing of it pinned, while one whose line names a session the window
/// holds exited goes. NEGATIVE CONTROL: the rule that took a line's session for gone whenever
/// the window's sessions are known pins and removes the first too.
#[test]
fn a_worktree_a_stranger_session_built_in_waits_seven_days() {
    let s = scratch_with_origin();
    let stranger = s.wt("stranger");
    std::fs::write(stranger.join("Cargo.toml"), "[workspace] # stranger\n").expect("edited");
    let exited = s.wt("exited");
    std::fs::write(exited.join("Cargo.toml"), "[workspace] # exited\n").expect("edited");
    let now = SystemTime::now() + RECORDS_QUIET + Duration::from_secs(10 * 60);
    let (done, _) = run(
        &s,
        &Setup {
            owed: Some(owed_file(&s)),
            now: Some(now),
            roster: Some(vec![SessionSeen {
                sid: sid(2),
                live: false,
            }]),
            starts: vec![start(&sid(3), &stranger), start(&sid(2), &exited)],
            ..Setup::default()
        },
    );
    let why = kept_for(&done, &stranger);
    assert!(why.contains("was written"), "{why}");
    assert!(stranger.join("Cargo.toml").is_file() && s.listed(&stranger));
    assert!(pins_of(&s.checkout(), "stranger").is_empty());
    assert!(went(&done, &exited), "{done:#?}");
}

/// AN OWED PUSH WHOSE NAME ORIGIN HOLDS AT ANOTHER COMMIT IS PARKED: a worktree whose HEAD
/// `origin/main` lacks goes once its orphan pin stands in its checkout, but its origin already
/// holds a ref of that name at another commit: the push sends nothing, leaves that ref as it
/// was, owes nothing and is journalled as parked. NEGATIVE CONTROLS: a push by force moves the
/// origin's ref; one that took the ref for pushed (no read before) writes no `parked` row.
#[test]
fn an_owed_push_whose_name_origin_holds_at_another_commit_is_parked() {
    let s = scratch_with_origin();
    let wt = s.wt("taken");
    std::fs::write(wt.join("work.rs"), "// work\n").expect("written");
    git(&wt, &["add", "work.rs"]);
    git(&wt, &["commit", "-qm", "work"]);
    let head = out(&wt, &["rev-parse", "HEAD"]);
    let name = format!("refs/orphans/aterm-taken/{}", &head[..12]);
    let main = out(&origin(&s), &["rev-parse", "refs/heads/main"]);
    git(&origin(&s), &["update-ref", &name, &main]);
    let (done, rows) = run(
        &s,
        &Setup {
            owed: Some(owed_file(&s)),
            ..Setup::default()
        },
    );
    assert!(went(&done, &wt), "{done:#?}");
    assert_eq!(s.rev(&s.checkout(), &name), Some(head));
    assert_eq!(
        s.rev(&origin(&s), &name),
        Some(main),
        "origin's ref is as it was"
    );
    assert!(done.owed.is_empty() && done.pushed == 0, "{done:#?}");
    assert_eq!(done.parked.len(), 1, "{done:#?}");
    assert!(
        pins::load(&owed_file(&s))
            .expect("the owed file")
            .is_empty()
    );
    let parked: Vec<Value> = rows
        .iter()
        .map(|r| aterm_json::from_str::<Value>(r).expect("a JSON row"))
        .filter(|v| v["kind"].as_str() == Some(crate::harness::disk::WORKTREE_PUSH_ROW_KIND))
        .filter(|v| v["action"].as_str() == Some("parked"))
        .collect();
    assert_eq!(parked.len(), 1, "{rows:#?}");
}

/// A MERGED, CLEAN, QUIET WORKTREE OUTSIDE AN AGENT'S SHAPE GIVES BACK ITS BUILD DIRECTORY AND
/// STAYS (DC-21): a person's worktree whose commit `origin/main` took, clean and a week quiet,
/// loses the shared cache's build directory that mirrors it and the one of a workspace nested
/// in it, each a `build-cache-sweep` intent row and then its outcome, while the worktree is
/// still listed on its branch, every entry of its tree and of git's own folder for it (its
/// `index`, `HEAD` and `logs/HEAD` among them) is byte for byte and to the instant what it was
/// after every run, no worktree row or receipt is written, and the worktree remover still
/// refuses it; the build directories of the main checkout and of a path that only starts with
/// the worktree's stand. Before that, with the ledger refusing rows nothing goes; while a
/// process may use the worktree nothing of it goes; a worktree edited after the step judged it
/// keeps its build directory; one a process comes to use after the step's first question of
/// it keeps both with no intent on record; with the pass lock lost as the first intent goes on
/// record that intent closes as kept and nothing goes; and a process in one build directory
/// keeps that one, with no intent on record, while the nested one goes. Once its mirror is
/// gone the worktree is not judged again. NEGATIVE CONTROLS: without the build-directory arm
/// of the judgment the default run takes nothing; without the write-ahead check the first run
/// takes both; without the question whether a process uses the worktree the second run takes
/// them; without the second judgment that run takes the edited one's; without the question
/// asked again before each intent the third run takes both; without the remover's second read
/// of its leave the fourth run takes one; without the cache remover's process fence the fifth
/// run takes the top one; with the mirror read as a string prefix the sibling's goes; without
/// the mirror check the last run still says its build directory goes; a step that wrote the
/// worktree's index, or anything else of it, fails the comparison after its run.
#[test]
fn a_merged_clean_quiet_worktree_outside_an_agents_shape_gives_back_its_build_directory_and_stays()
{
    let s = scratch();
    let w = s.person_wt("done");
    std::fs::write(w.join("x.rs"), "\n").expect("written");
    git(&w, &["add", "x.rs"]);
    git(&w, &["commit", "-qm", "x"]);
    let x = s.rev(&w, "HEAD").expect("its commit");
    git(
        &s.checkout(),
        &["update-ref", "refs/remotes/origin/main", &x],
    );
    let leaf = s.leaf(&w);
    let nested = s.leaf(&w.join("crates/tool"));
    let sibling = s.leaf(&s.home.join("aterm-wt-done2"));
    let main_leaf = s.leaf(&s.checkout());
    let late = s.person_wt("late");
    let late_leaf = s.leaf(&late);
    let artifact = Path::new("debug/deps/libx.rlib");
    let git_before = git_files(&w);
    let before = standing(&w);

    let (done, rows) = run(
        &s,
        &Setup {
            journal_off: true,
            ..Setup::default()
        },
    );
    assert_eq!(standing(&w), before, "after the unjournalled run");
    assert!(
        leaf.join(artifact).is_file() && nested.join(artifact).is_file(),
        "unjournalled, they stand: {done:#?}"
    );
    assert!(
        done.refused.contains(&Refusal::Unjournalled(w.clone())),
        "{done:#?}"
    );
    assert!(done.given_back.is_empty() && rows.is_empty(), "{done:#?}");
    assert!(s.listed(&w), "it is listed");

    let (done, rows) = run(
        &s,
        &Setup {
            using: Some(w.clone()),
            edits_late: Some(late.clone()),
            ..Setup::default()
        },
    );
    for stands in [&leaf, &nested, &late_leaf] {
        assert!(stands.join(artifact).is_file(), "{done:#?}");
    }
    let why = kept_for(&done, &w);
    assert!(why.contains("active process use"), "{why}");
    let why = kept_for(&done, &late);
    assert!(why.contains("late.rs"), "{why}");
    assert!(sweep_rows(&rows).is_empty(), "{rows:#?}");
    assert_eq!(
        standing(&w),
        before,
        "after the run a process may use it in"
    );
    let late_before = standing(&late);

    let (done, rows) = run(
        &s,
        &Setup {
            using_late: Some(w.clone()),
            ..Setup::default()
        },
    );
    for stands in [&leaf, &nested, &late_leaf] {
        assert!(stands.join(artifact).is_file(), "{done:#?}");
    }
    let why = kept_for(&done, &w);
    assert!(
        why.contains("a process came to use it") && why.contains("so does its shared build"),
        "{why}"
    );
    assert!(done.given_back.is_empty(), "{done:#?}");
    assert!(sweep_rows(&rows).is_empty(), "no intent: {rows:#?}");
    assert_eq!(
        standing(&w),
        before,
        "after the run a process came to use it in"
    );

    let (done, rows) = run(
        &s,
        &Setup {
            lock_lost_at_intent: true,
            ..Setup::default()
        },
    );
    for stands in [&leaf, &nested, &late_leaf] {
        assert!(
            stands.join(artifact).is_file(),
            "the lock was lost: {done:#?}"
        );
    }
    let mut closed: Vec<Vec<String>> = [&leaf, &nested]
        .iter()
        .map(|path| sweeps_of(&rows, path))
        .collect();
    closed.sort();
    assert_eq!(
        closed,
        [vec![], vec!["removing".to_owned(), "kept".to_owned()]]
    );
    assert_eq!(sweep_rows(&rows).len(), 2, "{rows:#?}");
    assert!(done.given_back.is_empty(), "{done:#?}");
    assert_eq!(
        standing(&w),
        before,
        "after the run that lost the pass lock"
    );

    let (done, rows) = run(
        &s,
        &Setup {
            busy: Some(leaf.join("debug")),
            ..Setup::default()
        },
    );
    assert!(leaf.join(artifact).is_file(), "it is in use: {done:#?}");
    assert!(sweeps_of(&rows, &leaf).is_empty(), "no intent: {rows:#?}");
    assert!(nested.symlink_metadata().is_err(), "{done:#?}");
    assert_eq!(sweeps_of(&rows, &nested), ["removing", "removed"]);
    assert_eq!(done.given_back.len(), 1, "{done:#?}");
    assert_eq!(s.leaf(&w.join("crates/tool")), nested, "laid again");
    assert_eq!(standing(&w), before, "after the run a process built in");

    let (done, rows) = run(&s, &Setup::default());
    for gone in [&leaf, &nested] {
        assert!(gone.symlink_metadata().is_err(), "{done:#?}");
        assert!(gone.ends_with(".cargo-build"), "{}", gone.display());
        assert_eq!(sweeps_of(&rows, gone), ["removing", "removed"]);
    }
    assert_eq!(sweep_rows(&rows).len(), 4, "{rows:#?}");
    for stands in [&sibling, &main_leaf, &late_leaf] {
        assert!(stands.join(artifact).is_file(), "{}", stands.display());
    }
    assert!(s.listed(&w) && w.join("x.rs").is_file(), "it stays");
    assert_eq!(s.rev(&s.checkout(), "refs/heads/wt/done"), Some(x));
    assert_eq!(git_files(&w), git_before, "git's own files for it");
    assert_eq!(standing(&w), before, "after the run that took them");
    assert_eq!(
        standing(&late),
        late_before,
        "the edited one, as its edit left it"
    );
    assert_eq!(s.receipts(), Vec::<PathBuf>::new());
    assert!(
        done.removed.is_empty() && worktree_rows(&rows).is_empty(),
        "{done:#?}"
    );
    let mut gave: Vec<&Path> = done.given_back.iter().map(|g| g.path.as_path()).collect();
    gave.sort_unstable();
    let mut both = [leaf.as_path(), nested.as_path()];
    both.sort_unstable();
    assert_eq!(gave, both, "{done:#?}");
    assert!(
        done.given_back
            .iter()
            .all(|g| g.worktree == w && matches!(g.why, Unused::MergedWorktree { .. })),
        "{done:#?}"
    );
    assert!(done.freed() >= 128 * 1024, "{done:#?}");
    let why = kept_for(&done, &w);
    assert!(
        why.contains("only its shared build directory goes"),
        "{why}"
    );
    let clear = |_: &[PathBuf], _: &[u32]| Ok(PathScan::default());
    let main = s.checkout();
    let refused =
        retired::remove_worktree_with_processes(&GitCli, &s.home, &main, &w, s.now, &clear);
    let refused = refused.expect_err("no remover takes a person's worktree");
    assert!(refused.contains("a person's worktree stays"), "{refused}");
    assert!(s.listed(&w) && w.join("x.rs").is_file(), "it stays");

    let (done, rows) = run(&s, &Setup::default());
    assert!(sweep_rows(&rows).is_empty(), "{rows:#?}");
    let why = kept_for(&done, &w);
    assert!(why.contains("a person's worktree stays"), "{why}");
    assert_eq!(standing(&w), before, "after the last run");
}

/// A WORKTREE OUTSIDE AN AGENT'S SHAPE WRITTEN WITHIN SEVEN DAYS KEEPS ITS BUILD DIRECTORY
/// (DC-21): of six persons' worktrees, each merged and clean with a build directory in the
/// shared cache, one with a tracked file written six days ago, one whose build directory holds
/// a file written yesterday, one whose reflog of HEAD git wrote two days ago, one a
/// build-start line names within the week (a build that compiled nothing writes no file) and
/// one a build starts in after the step's look read the record each keep their build
/// directory, by the judgment's own words and with no row on record, while the control's goes
/// though a build-start line names it eight days ago; all six stay listed on their branches,
/// every entry of each and of git's own folder for it as it was. NEGATIVE CONTROLS: without
/// the tree walk A's build directory goes; without the build-folder clause B reaches the
/// cache's remover, which keeps it without a word of the judgment's; without git's own files
/// counted D's goes; without the build-start lines read E's goes; without the record read
/// again before the second judgment F's goes; with every line that names a worktree counted,
/// whatever its date, the control's stays; a step that did nothing fails on the control.
#[test]
fn a_worktree_outside_an_agents_shape_written_within_seven_days_keeps_its_build_directory() {
    let s = scratch();
    let a = s.person_wt("a");
    let b = s.person_wt("b");
    let d = s.person_wt("d");
    let e = s.person_wt("e");
    let f = s.person_wt("f");
    let c = s.person_wt("c");
    let kept = [&a, &b, &d, &e, &f].map(|wt| s.leaf(wt));
    let leaf_c = s.leaf(&c);
    let artifact = Path::new("debug/deps/libx.rlib");
    s.written(&a.join("Cargo.toml"), 6 * DAY);
    s.written(&kept[1].join(artifact), DAY);
    s.written(&git_entry(&d).join("logs").join("HEAD"), 2 * DAY);
    let built = atpkg::build_start::Start {
        at_ms: unix_ms(s.now - DAY),
        ..start(&sid(4), &e)
    };
    let built_long_ago = atpkg::build_start::Start {
        at_ms: unix_ms(s.now - UNWRITTEN - DAY),
        ..start(&sid(5), &c)
    };
    let all = [&a, &b, &d, &e, &f, &c];
    let before = all.map(|wt| standing(wt));

    let (done, rows) = run(
        &s,
        &Setup {
            starts: vec![built, built_long_ago],
            builds_late: Some(f.clone()),
            ..Setup::default()
        },
    );
    assert_eq!(all.map(|wt| standing(wt)), before, "each as it was");
    assert!(leaf_c.symlink_metadata().is_err(), "the control: {done:#?}");
    for leaf in &kept {
        assert!(leaf.join(artifact).is_file(), "{}", leaf.display());
    }
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
    let why = kept_for(&done, &e);
    assert!(
        why.contains(&sid(4)) && why.contains("started a build in it"),
        "{why}"
    );
    let why = kept_for(&done, &f);
    assert!(
        why.contains(&sid(9)) && why.contains("started a build in it"),
        "{why}"
    );
    assert_eq!(sweeps_of(&rows, &leaf_c), ["removing", "removed"]);
    assert_eq!(sweep_rows(&rows).len(), 2, "{rows:#?}");
    assert!(leaf_c.ends_with("aterm-wt-c/.cargo-build"));
    for (wt, name) in all.into_iter().zip("abdefc".chars()) {
        assert!(s.listed(wt), "{name} is listed");
        assert!(
            s.rev(&s.checkout(), &format!("refs/heads/wt/{name}"))
                .is_some(),
            "{name}'s branch stays"
        );
    }
}

/// A WORKTREE WITH COMMITS MAIN LACKS KEEPS ITS BUILD DIRECTORY (DC-21): a person's worktree
/// with a commit `origin/main` lacks, a week quiet and with the pinned rule on, keeps its build
/// directory and is itself as it was: listed, its branch at its commit, every entry of its
/// tree and of git's own folder for it unchanged, no pin of it in its checkout or its origin,
/// no row and no receipt; the control's build directory goes.
/// NEGATIVE CONTROLS: without the merged clause its build directory goes; with the pinned rule
/// opened to a worktree outside an agent's shape its commit is pinned on `refs/orphans` and it
/// is gone.
#[test]
fn a_worktree_with_commits_main_lacks_keeps_its_build_directory() {
    let s = scratch_with_origin();
    let ahead = s.person_wt("s15fab");
    std::fs::write(ahead.join("new.rs"), "\n").expect("written");
    git(&ahead, &["add", "new.rs"]);
    git(&ahead, &["commit", "-qm", "new"]);
    let tip = s.rev(&ahead, "HEAD").expect("its commit");
    let leaf = s.leaf(&ahead);
    let c = s.person_wt("c");
    let leaf_c = s.leaf(&c);
    let before = standing(&ahead);

    let (done, rows) = run(
        &s,
        &Setup {
            owed: Some(owed_file(&s)),
            ..Setup::default()
        },
    );
    assert_eq!(standing(&ahead), before, "it is as it was");
    assert!(leaf_c.symlink_metadata().is_err(), "the control: {done:#?}");
    assert!(leaf.join("debug/deps/libx.rlib").is_file(), "{done:#?}");
    let why = kept_for(&done, &ahead);
    assert!(why.contains("is not in origin/main"), "{why}");
    assert!(s.listed(&ahead) && ahead.join("new.rs").is_file());
    assert_eq!(s.rev(&s.checkout(), "refs/heads/wt/s15fab"), Some(tip));
    for (dir, prefix) in [
        (s.checkout(), "refs/orphans"),
        (s.checkout(), "refs/snapshots"),
        (origin(&s), "refs/orphans"),
    ] {
        let found = refs(&dir, prefix);
        assert!(found.is_empty(), "under {prefix}: {found:?}");
    }
    assert!(sweeps_of(&rows, &leaf).is_empty(), "{rows:#?}");
    assert!(worktree_rows(&rows).is_empty(), "{rows:#?}");
    assert_eq!(s.receipts(), Vec::<PathBuf>::new());
}

/// `disk.auto_free_gib = 0`, OR A `[disk]` TABLE ATERM CANNOT READ, KEEPS EVERY BUILD DIRECTORY
/// (DC-21): through the host's own step, its process questions answered by an inert probe, the
/// build directory of a person's merged, clean, quiet worktree stays, with no ledger row, under
/// the floor at zero and under the configuration an aterm.toml whose `disk` is no table reads
/// as (every removal off); it stays, with no sweep row, where `disk.idle_build_dirs` is off,
/// where `disk.idle_build_days` asks a wait longer than its eight days, and where
/// `disk.idle_build_days = 1` asks less than seven days of a directory written three days ago.
/// NEGATIVE CONTROLS: the same step under the default configuration then takes it, its intent
/// and its outcome in the ledger, so each of the five kept it by its configuration; with the
/// window read off `idle_build_days` alone the one-day case takes it.
#[test]
fn auto_free_gib_zero_or_an_unreadable_disk_table_keeps_every_build_directory() {
    use crate::harness::disk::Config;

    let s = scratch();
    let c = s.person_wt("c");
    let leaf = s.leaf(&c);
    let artifact = leaf.join("debug/deps/libx.rlib");
    let host = Host::of(&s);
    let ledger = host.state.join(crate::harness::cli::DISK_LEDGER);
    let host_step = |config: Config| {
        let mut schedule = Schedule::new();
        let done = host.step(&s, config, &mut schedule);
        (done, schedule)
    };

    let zero = Config {
        auto_free_gib: 0,
        ..Config::default()
    };
    let (done, schedule) = host_step(zero);
    assert!(done.given_back.is_empty(), "{done:#?}");
    assert!(done.kept.is_empty(), "nothing was judged: {done:#?}");
    assert!(artifact.is_file(), "the floor is zero");
    assert!(ledger.symlink_metadata().is_err(), "no ledger row");
    assert!(schedule.due(Instant::now(), || false), "the step stays due");

    let toml = s.home.join("aterm.toml");
    std::fs::write(&toml, "disk = 3\n").expect("written");
    let reading = crate::harness::cli::disk_config_read(Some(&toml));
    assert!(reading.off_by_fault, "{reading:#?}");
    assert_eq!(reading.config.auto_free_gib, 0, "{reading:#?}");
    let (done, _) = host_step(reading.config);
    assert!(done.given_back.is_empty(), "{done:#?}");
    assert!(done.kept.is_empty(), "nothing was judged: {done:#?}");
    assert!(artifact.is_file(), "the table cannot be read");
    assert!(ledger.symlink_metadata().is_err(), "no ledger row");

    let off = Config {
        idle_build_dirs: false,
        ..Config::default()
    };
    let wait = Config {
        idle_build_days: 30,
        ..Config::default()
    };
    let short = Config {
        idle_build_days: 1,
        ..Config::default()
    };
    for (config, what) in [(off, "idle_build_dirs"), (wait, "idle_build_days")] {
        let (done, _) = host_step(config);
        assert!(done.given_back.is_empty(), "{what}: {done:#?}");
        assert!(artifact.is_file(), "{what} keeps it");
        assert!(sweep_rows(&host.ledger()).is_empty(), "{what}");
        assert!(s.listed(&c), "{what}: it is listed");
    }

    s.written(&artifact, 3 * DAY);
    let (done, _) = host_step(short);
    assert!(done.given_back.is_empty(), "{done:#?}");
    assert!(artifact.is_file(), "seven days is the least wait");
    let why = kept_for(&done, &c);
    assert!(why.contains("was written within 7d"), "{why}");
    assert!(sweep_rows(&host.ledger()).is_empty(), "no sweep row");

    s.written(&artifact, UNWRITTEN + DAY);
    let (done, _) = host_step(Config::default());
    assert!(leaf.symlink_metadata().is_err(), "{done:#?}");
    assert_eq!(done.given_back.len(), 1, "{done:#?}");
    assert_eq!(sweeps_of(&host.ledger(), &leaf), ["removing", "removed"]);
    assert!(s.listed(&c), "it stays");
}

/// A BUILD DIRECTORY THE STEP JUDGED AND KEPT RESTS A DAY (DC-21): a person's merged, clean
/// worktree whose build directory was written three days ago is judged and its build directory
/// kept; with that file old again, the next step under the same schedule does not judge it for
/// its build directory (it stays as any person's worktree does, with no sweep row), and a step
/// under a schedule that holds no rest takes it. The schedule holds a rest for less than
/// [`BUILD_DIR_EVERY`] and forgets it then. NEGATIVE CONTROL: without the rest the second step
/// takes the build directory.
#[test]
fn a_build_directory_judged_and_kept_rests_a_day() {
    use crate::harness::disk::Config;

    let s = scratch();
    let c = s.person_wt("c");
    let leaf = s.leaf(&c);
    let artifact = leaf.join("debug/deps/libx.rlib");
    let host = Host::of(&s);
    s.written(&artifact, 3 * DAY);

    let mut schedule = Schedule::new();
    let done = host.step(&s, Config::default(), &mut schedule);
    assert_eq!(done.build_kept, [c.clone()], "{done:#?}");
    let why = kept_for(&done, &c);
    assert!(why.contains("its build folder"), "{why}");

    s.written(&artifact, UNWRITTEN + DAY);
    let done = host.step(&s, Config::default(), &mut schedule);
    assert!(artifact.is_file(), "it rests: {done:#?}");
    assert!(
        done.given_back.is_empty() && done.build_kept.is_empty(),
        "{done:#?}"
    );
    let why = kept_for(&done, &c);
    assert!(why.contains("a person's worktree stays"), "{why}");
    assert!(!why.contains("build directory"), "{why}");
    assert!(sweep_rows(&host.ledger()).is_empty(), "no sweep row");

    let done = host.step(&s, Config::default(), &mut Schedule::new());
    assert!(leaf.symlink_metadata().is_err(), "{done:#?}");
    assert_eq!(sweeps_of(&host.ledger(), &leaf), ["removing", "removed"]);
    assert!(s.listed(&c), "it stays");

    let t0 = Instant::now();
    let mut rests = Schedule::new();
    rests.rest(t0, std::slice::from_ref(&c));
    let last = t0 + BUILD_DIR_EVERY - Duration::from_secs(1);
    assert_eq!(rests.rested(last), [c.clone()]);
    assert!(rests.rested(t0 + BUILD_DIR_EVERY).is_empty(), "a day on");
    rests.rest(t0 + BUILD_DIR_EVERY, &[]);
    assert!(
        rests.rested(t0).is_empty(),
        "a rest that passed is forgotten"
    );
}
