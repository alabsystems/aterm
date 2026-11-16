// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The branch-name step on a scratch home: a main checkout `aterm` whose `origin` is a bare
//! `origin.git` beside it, `main` pushed and fetched. A branch the server had and no longer
//! has is pushed from a commit made on `main`'s tree (no file of the checkout changes),
//! fetched, then deleted in `origin.git` alone, as a merged pull request's branch is: the
//! checkout keeps its remote-tracking name, as one nobody fetched with `--prune` does. Git is
//! the real one; agh never runs. Each case carries a control that the same step must leave as
//! it is, or a run that must do nothing, so a step that did nothing, or did everything, fails.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use aterm_json::Value;
use aterm_spec::wait::MUST_HAPPEN;
use atpkg::build_cache::retired::{Answer, Git, GitCli};

use super::{Done, Facts, Hooks, step};
use crate::harness::disk::worktree::pins;
use crate::harness::disk::{REF_ROW_KIND, WORKTREE_PUSH_ROW_KIND};

/// A scratch home, removed when the test ends.
struct Scratch {
    _dir: aterm_tempfile::TempDir,
    home: PathBuf,
}

impl Scratch {
    fn checkout(&self) -> PathBuf {
        self.home.join("aterm")
    }

    fn origin(&self) -> PathBuf {
        self.home.join("origin.git")
    }

    fn owed(&self) -> PathBuf {
        self.home.join("state").join(pins::OWED_FILE)
    }

    /// A new commit over `main`, on `main`'s own tree: a commit `origin/main` lacks.
    fn commit(&self, message: &str) -> String {
        out(
            &self.checkout(),
            &["commit-tree", "-p", "main", "-m", message, "main^{tree}"],
        )
    }

    /// `main` moved to `oid` (a commit over it, on its tree), pushed and fetched: `origin/main`
    /// holds it.
    fn advance_main(&self, oid: &str) {
        git(&self.checkout(), &["update-ref", "refs/heads/main", oid]);
        git(&self.checkout(), &["push", "-q", "origin", "main"]);
        git(&self.checkout(), &["fetch", "-q", "origin"]);
    }

    /// The server's branch `branch` at `oid`, fetched into the checkout's remote-tracking name.
    fn on_server(&self, branch: &str, oid: &str) {
        git(
            &self.checkout(),
            &[
                "push",
                "-q",
                "origin",
                &format!("{oid}:refs/heads/{branch}"),
            ],
        );
        git(&self.checkout(), &["fetch", "-q", "origin"]);
    }

    /// The server deletes its branch `branch`; the checkout's remote-tracking name stays.
    fn deleted_on_server(&self, branch: &str) {
        git(
            &self.origin(),
            &["update-ref", "-d", &format!("refs/heads/{branch}")],
        );
    }

    /// The commit `name` names in the repository `dir`, `None` when no ref of that name stands.
    fn rev(dir: &Path, name: &str) -> Option<String> {
        let a = GitCli.run(dir, &["rev-parse", "--verify", "--quiet", name]);
        (a.code == Some(0)).then(|| a.out.trim().to_owned())
    }

    /// Every ref of the repository `dir` under `prefix`, `(name, commit)`.
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
}

/// `git <args>` in `dir`, as a person set the scratch up: its output, trimmed; it must succeed.
#[track_caller]
fn out(dir: &Path, args: &[&str]) -> String {
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
        "-c",
        "fetch.prune=false",
        "-c",
        "core.hooksPath=/dev/null",
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
    a.out.trim().to_owned()
}

/// [`out`], its output dropped.
#[track_caller]
fn git(dir: &Path, args: &[&str]) {
    out(dir, args);
}

fn scratch() -> Scratch {
    let dir = aterm_tempfile::Builder::new()
        .prefix("disk-refs-")
        .suffix(".noindex")
        .tempdir()
        .expect("scratch");
    let home = std::fs::canonicalize(dir.path())
        .expect("resolved")
        .join("home");
    let checkout = home.join("aterm");
    let origin = home.join("origin.git");
    std::fs::create_dir_all(&checkout).expect("a repo");
    git(&checkout, &["init", "-q"]);
    std::fs::write(checkout.join("Cargo.toml"), "[workspace]\n").expect("written");
    git(&checkout, &["add", "Cargo.toml"]);
    git(&checkout, &["commit", "-qm", "main"]);
    git(&home, &["init", "-q", "--bare", &origin.to_string_lossy()]);
    git(
        &checkout,
        &["remote", "add", "origin", &origin.to_string_lossy()],
    );
    git(&checkout, &["push", "-q", "origin", "main"]);
    git(&checkout, &["fetch", "-q", "origin"]);
    Scratch { _dir: dir, home }
}

/// What one step is run with beyond the scratch.
#[derive(Default)]
struct Setup {
    /// The branches live sessions used, `None`: not known.
    used: Option<Vec<String>>,
    /// The ledger refuses every row of this action.
    refuse: Option<&'static str>,
    /// How long after now the step runs, in seconds.
    later: u64,
    /// The origin's refs read back after a push as another commit than was pushed.
    misread: bool,
}

/// The real git, watched: every `git push` is checked against the owed file at `owed`, which
/// must already owe each ref it sends (a push is on record before it goes), and with `misread`
/// every read of the origin's refs after a push answers another commit.
struct Watched<'a> {
    owed: &'a Path,
    misread: bool,
    pushed: std::cell::Cell<bool>,
    unowed: RefCell<Vec<String>>,
}

impl Watched<'_> {
    fn watch(&self, args: &[&str], answer: Answer) -> Answer {
        if args.contains(&"push") && args.contains(&"--porcelain") {
            self.pushed.set(true);
            let owed = pins::load(self.owed).unwrap_or_default();
            for spec in args.iter().filter(|a| a.contains(":refs/")) {
                let (oid, name) = spec.split_once(':').expect("a refspec");
                let on_record = owed
                    .iter()
                    .any(|o| o.push.refs.contains(&[name.to_owned(), oid.to_owned()]));
                if !on_record {
                    self.unowed.borrow_mut().push((*spec).to_owned());
                }
            }
        }
        if self.misread && self.pushed.get() && args.contains(&"ls-remote") {
            let out = answer
                .out
                .lines()
                .filter_map(|l| l.split_once('\t'))
                .map(|(_, name)| format!("{}\t{name}\n", "f".repeat(40)))
                .collect();
            return Answer { out, ..answer };
        }
        answer
    }
}

impl Git for Watched<'_> {
    fn run(&self, dir: &Path, args: &[&str]) -> Answer {
        self.watch(args, GitCli.run(dir, args))
    }

    fn run_with_alt(&self, dir: &Path, args: &[&str], alt: Option<&Path>) -> Answer {
        self.watch(args, GitCli.run_with_alt(dir, args, alt))
    }

    fn run_env(
        &self,
        dir: &Path,
        args: &[&str],
        index: Option<&Path>,
        env: &[(&str, &str)],
    ) -> Answer {
        self.watch(args, GitCli.run_env(dir, args, index, env))
    }

    fn read_history(
        &self,
        dir: &Path,
        args: &[&str],
        input: &[u8],
        objects: Option<&Path>,
        deadline: Instant,
    ) -> Answer {
        GitCli.read_history(dir, args, input, objects, deadline)
    }
}

/// One step over the main checkouts discovery finds in the scratch home: what it did, and the
/// rows it put on record. Every `pruning` row must reach the ledger while the name it names
/// still stands, and every push must be owed before it is sent.
fn run(s: &Scratch, setup: &Setup) -> (Done, Vec<String>) {
    let checkouts = crate::harness::disk::discover::main_checkouts(&s.home);
    assert_eq!(checkouts, vec![s.checkout()], "the one checkout is found");
    let owed = s.owed();
    let facts = Facts {
        checkouts: &checkouts,
        used: setup.used.as_deref(),
        owed: &owed,
    };
    let rows = RefCell::new(Vec::new());
    let early = RefCell::new(Vec::new());
    let mut journal = |row: &str| {
        let v = parsed(row);
        if v["kind"].as_str() == Some(REF_ROW_KIND) && action_of(row) == "pruning" {
            let name = v["ref"].as_str().expect("a pruning row names its ref");
            let stands = if name.starts_with("worktrees/") {
                s.checkout().join(".git").join(name).is_dir()
            } else {
                Scratch::rev(&s.checkout(), name).is_some()
            };
            if !stands {
                early.borrow_mut().push(name.to_owned());
            }
        }
        if let Some(action) = setup.refuse
            && action_of(row) == action
        {
            return false;
        }
        rows.borrow_mut().push(row.to_owned());
        true
    };
    let agh = |_: &[&str]| -> Result<std::process::Output, String> {
        Err("no agh runs in a test".to_owned())
    };
    let git = Watched {
        owed: &owed,
        misread: setup.misread,
        pushed: std::cell::Cell::new(false),
        unowed: RefCell::new(Vec::new()),
    };
    let done = step(
        &git,
        &facts,
        SystemTime::now() + std::time::Duration::from_secs(setup.later),
        Instant::now() + MUST_HAPPEN,
        Hooks {
            still_held: &|| true,
            journal: &mut journal,
            agh: &agh,
            drop_gone: &|checkout, custody| {
                atpkg::build_cache::gone_entries::drop_gone_with_processes(
                    &git,
                    checkout,
                    Some(&s.home),
                    None,
                    custody,
                    &|_, _| Ok(aterm_uds::holders::PathScan::default()),
                )
            },
        },
    );
    assert_eq!(done.unfinished, None, "the step ran through: {done:#?}");
    assert!(
        early.borrow().is_empty(),
        "a pruning row reached the ledger after its name went: {early:?}"
    );
    assert!(
        git.unowed.borrow().is_empty(),
        "a push went before the owed file held it: {:?}",
        git.unowed.borrow()
    );
    (done, rows.into_inner())
}

/// Two days: past [`super::QUIET`], so a branch made now is judged.
const TWO_DAYS: u64 = 2 * 24 * 60 * 60;

fn parsed(row: &str) -> Value {
    aterm_json::from_str(row).expect("a JSON row")
}

fn action_of(row: &str) -> String {
    parsed(row)["action"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// The `(kind, action)` of each row that names `name` (a `branch-ref` row's `ref`, a
/// `worktree-push` row's refs), in the order they went on record.
fn rows_of(rows: &[String], name: &str) -> Vec<(String, String)> {
    rows.iter()
        .map(|r| parsed(r))
        .filter(|v| {
            v["ref"].as_str() == Some(name)
                || v["refs"].as_array().is_some_and(|refs| {
                    refs.iter()
                        .any(|r| r.as_str().is_some_and(|r| r.starts_with(name)))
                })
        })
        .map(|v| {
            (
                v["kind"].as_str().unwrap_or_default().to_owned(),
                v["action"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

fn row(kind: &str, action: &str) -> (String, String) {
    (kind.to_owned(), action.to_owned())
}

/// A STALE NAME WHOSE COMMITS MAIN HAS IS PRUNED: the server deleted `agent/merged` after
/// `main` took its commit, and the checkout's `origin/agent/merged` goes, its `pruning` row on
/// record before its `pruned` one (and while the name still stands), with no pin. NEGATIVE
/// CONTROLS: a run whose origin cannot be read prunes nothing (which names the server lacks is
/// read from the server, never guessed), nor does one whose origin answers a listing with no
/// main (an empty or partial answer is no listing); a ledger that takes no `pruning` row leaves
/// the name; `origin/agent/alive`, whose branch the server still has, a tag at the same commit
/// and `origin/main` stay.
#[test]
fn a_stale_name_whose_commits_main_has_is_pruned() {
    let s = scratch();
    let tip = s.commit("merged");
    s.on_server("agent/merged", &tip);
    s.on_server("agent/alive", &tip);
    s.advance_main(&tip);
    git(&s.checkout(), &["tag", "v1", &tip]);
    s.deleted_on_server("agent/merged");
    let stale = "refs/remotes/origin/agent/merged";
    let alive = "refs/remotes/origin/agent/alive";

    let gone = s.home.join("gone.git");
    git(
        &s.checkout(),
        &["remote", "set-url", "origin", &gone.to_string_lossy()],
    );
    let (done, rows) = run(&s, &Setup::default());
    assert!(
        done.pruned.is_empty() && rows.is_empty(),
        "{done:#?} {rows:?}"
    );
    assert_eq!(done.unread.len(), 1, "{done:#?}");
    assert_eq!(Scratch::rev(&s.checkout(), stale), Some(tip.clone()));
    git(
        &s.checkout(),
        &["remote", "set-url", "origin", &s.origin().to_string_lossy()],
    );

    // An origin that answers with no main is read as no listing: nothing goes by it.
    git(&s.origin(), &["update-ref", "-d", "refs/heads/main"]);
    let (done, rows) = run(&s, &Setup::default());
    git(&s.origin(), &["update-ref", "refs/heads/main", &tip]);
    assert!(
        done.pruned.is_empty() && rows.is_empty(),
        "{done:#?} {rows:?}"
    );
    assert!(
        done.unread
            .iter()
            .any(|(_, why)| why.contains("no refs/heads/main")),
        "{done:#?}"
    );
    assert_eq!(Scratch::rev(&s.checkout(), stale), Some(tip.clone()));

    // A ledger that takes no `pruning` row: the name stays.
    let (done, _) = run(
        &s,
        &Setup {
            refuse: Some("pruning"),
            ..Setup::default()
        },
    );
    assert!(done.pruned.is_empty(), "{done:#?}");
    assert!(
        done.kept.iter().any(|(_, n, _)| n == stale),
        "kept for want of its row: {done:#?}"
    );
    assert_eq!(Scratch::rev(&s.checkout(), stale), Some(tip.clone()));

    let (done, rows) = run(&s, &Setup::default());
    assert_eq!(done.pruned.len(), 1, "{done:#?}");
    assert_eq!(done.pruned[0].name, stale);
    assert_eq!(done.pruned[0].pin, None);
    assert_eq!(Scratch::rev(&s.checkout(), stale), None);
    assert_eq!(
        rows_of(&rows, stale),
        vec![row(REF_ROW_KIND, "pruning"), row(REF_ROW_KIND, "pruned")]
    );
    assert_eq!(Scratch::rev(&s.checkout(), alive), Some(tip.clone()));
    assert!(rows_of(&rows, alive).is_empty(), "{rows:?}");
    assert_eq!(
        Scratch::rev(&s.checkout(), "refs/tags/v1"),
        Some(tip.clone())
    );
    assert_eq!(
        Scratch::rev(&s.checkout(), "refs/remotes/origin/main"),
        Some(tip.clone())
    );
    assert!(Scratch::refs(&s.checkout(), "refs/orphans/").is_empty());
    assert!(Scratch::refs(&s.origin(), "refs/orphans/").is_empty());
    assert_eq!(
        Scratch::rev(&s.origin(), "refs/heads/agent/alive"),
        Some(tip)
    );
}

/// THE ONLY NAME OF UNMERGED COMMITS IS PINNED UNDER `refs/orphans/` BEFORE ITS PRUNE: the
/// server deleted `agent/lost`, whose commit `origin/main` lacks and no other ref holds; its tip
/// goes on `refs/orphans/agent/lost/<12 hex digits>` in the checkout, the `pinning` row on
/// record before the pin and the `pruning` row before the prune, and only then does its
/// remote-tracking name go. NEGATIVE CONTROLS: a ledger that takes no `pinning` row leaves the
/// name and writes no pin (no intent, no change); `agent/shared`, whose unmerged commit a tag
/// also holds, is pruned with no pin, so a pin is written only for a last name; and
/// `agent/peeked`, whose commit only refs that go by themselves also hold (a session's peek, a
/// prefetch, a stash, another remote's name), is pinned like `agent/lost`, never pruned bare.
#[test]
fn the_only_name_of_unmerged_commits_is_pinned_under_refs_orphans_before_its_prune() {
    let s = scratch();
    let lost = s.commit("lost");
    let shared = s.commit("shared");
    let peeked = s.commit("peeked");
    s.on_server("agent/lost", &lost);
    s.on_server("agent/shared", &shared);
    s.on_server("agent/peeked", &peeked);
    git(&s.checkout(), &["tag", "keep-shared", &shared]);
    // Refs that go without anyone deciding to drop a commit: a session's peek, a prefetch, a
    // stash, another remote's name. None of them is a lasting name.
    for scratch_ref in [
        "refs/s595dac/peek-b",
        "refs/prefetch/remotes/origin/agent/peeked",
        "refs/stash",
        "refs/remotes/upstream/agent/peeked",
    ] {
        git(&s.checkout(), &["update-ref", scratch_ref, &peeked]);
    }
    s.deleted_on_server("agent/lost");
    s.deleted_on_server("agent/shared");
    s.deleted_on_server("agent/peeked");
    let name = "refs/remotes/origin/agent/lost";
    let pin = format!("refs/orphans/agent/lost/{}", &lost[..12]);
    let peeked_name = "refs/remotes/origin/agent/peeked";
    let peeked_pin = format!("refs/orphans/agent/peeked/{}", &peeked[..12]);

    let (done, _) = run(
        &s,
        &Setup {
            refuse: Some("pinning"),
            ..Setup::default()
        },
    );
    assert!(done.pinned.is_empty(), "{done:#?}");
    assert_eq!(Scratch::rev(&s.checkout(), name), Some(lost.clone()));
    assert_eq!(
        Scratch::rev(&s.checkout(), peeked_name),
        Some(peeked.clone()),
        "scratch refs hold nothing: it waits for its pin"
    );
    assert!(Scratch::refs(&s.checkout(), "refs/orphans/").is_empty());
    assert!(
        done.kept.iter().any(|(_, n, _)| n == name),
        "kept for want of its row: {done:#?}"
    );
    let shared_name = "refs/remotes/origin/agent/shared";
    assert_eq!(Scratch::rev(&s.checkout(), shared_name), None);
    assert!(
        done.pruned
            .iter()
            .any(|p| p.name == shared_name && p.pin.is_none()),
        "{done:#?}"
    );
    assert_eq!(
        Scratch::rev(&s.checkout(), "refs/tags/keep-shared"),
        Some(shared)
    );

    let (done, rows) = run(&s, &Setup::default());
    let mut pins_now = vec![(pin.clone(), lost.clone()), (peeked_pin.clone(), peeked)];
    pins_now.sort();
    assert_eq!(Scratch::refs(&s.checkout(), "refs/orphans/"), pins_now);
    assert_eq!(Scratch::rev(&s.checkout(), name), None);
    assert_eq!(Scratch::rev(&s.checkout(), peeked_name), None);
    assert!(
        done.pruned
            .iter()
            .any(|p| p.name == peeked_name && p.pin.as_deref() == Some(peeked_pin.as_str())),
        "{done:#?}"
    );
    assert_eq!(
        rows_of(&rows, name),
        vec![
            row(REF_ROW_KIND, "pinning"),
            row(REF_ROW_KIND, "pruning"),
            row(REF_ROW_KIND, "pruned"),
        ]
    );
    let pinning = rows
        .iter()
        .position(|r| action_of(r) == "pinning" && parsed(r)["ref"].as_str() == Some(name))
        .expect("a pinning row");
    assert_eq!(parsed(&rows[pinning])["pin"].as_str(), Some(pin.as_str()));
    let pruned = done.pruned.iter().find(|p| p.name == name).expect("pruned");
    assert_eq!(pruned.pin.as_deref(), Some(pin.as_str()));
}

/// AN ORPHAN PIN IS PUSHED AND CHECKED AGAINST ITS LOCAL COMMIT: every push is in the owed file
/// before it is sent; the pin of `agent/lost` stands on the origin under the same name at the
/// same commit, one `pushed` row says so, and nothing is owed. A push that fails (the origin's
/// `refs/orphans` takes no new ref) leaves the pin here, one `worktree-push` `owed` row and an
/// owed entry, and the name is still pruned; the worktree step's round then pushes it.
/// NEGATIVE CONTROLS: an owed file that cannot be read sends nothing and keeps the name (its pin
/// stays), and the next run pushes that pin before the name goes; a push whose ref reads back
/// at the origin as another commit stays owed, never `pushed`; where the origin already holds
/// that pin's name at another commit, the push is parked, never `pushed`, and the origin's ref
/// is not moved.
#[test]
fn an_orphan_pin_is_pushed_and_checked_against_its_local_commit() {
    use std::os::unix::fs::PermissionsExt as _;
    let s = scratch();
    let lost = s.commit("lost");
    s.on_server("agent/lost", &lost);
    s.deleted_on_server("agent/lost");
    let name = "refs/remotes/origin/agent/lost";
    let pin = format!("refs/orphans/agent/lost/{}", &lost[..12]);

    // The owed file cannot be read: the pin is made, but its push is on no record, so it is
    // not sent and the name stays.
    std::fs::create_dir_all(s.owed().parent().expect("a folder")).expect("made");
    std::fs::write(s.owed(), "not what it writes").expect("written");
    let (done, rows) = run(&s, &Setup::default());
    std::fs::remove_file(s.owed()).expect("removed");
    assert!(
        done.pushed.is_empty() && done.owed.is_empty() && done.pruned.is_empty(),
        "{done:#?}"
    );
    assert!(done.unrecorded.is_some(), "{done:#?}");
    assert_eq!(Scratch::rev(&s.checkout(), name), Some(lost.clone()));
    assert_eq!(Scratch::rev(&s.checkout(), &pin), Some(lost.clone()));
    assert_eq!(Scratch::rev(&s.origin(), &pin), None);
    assert_eq!(
        rows_of(&rows, name),
        vec![row(REF_ROW_KIND, "pinning"), row(REF_ROW_KIND, "kept")]
    );
    assert!(
        rows.iter().any(|r| action_of(r) == "unrecorded"),
        "{rows:?}"
    );

    // The next run finds the pin that run left standing, unpushed and owed nowhere: the name is
    // still the only lasting name of its commit beside its own pin, so the pin's push is
    // recorded and sent before the name goes.
    let (done, rows) = run(&s, &Setup::default());
    assert_eq!(done.pushed.len(), 1, "{done:#?}");
    assert!(done.owed.is_empty() && done.parked.is_empty(), "{done:#?}");
    assert_eq!(Scratch::rev(&s.origin(), &pin), Some(lost.clone()));
    assert_eq!(Scratch::rev(&s.checkout(), &pin), Some(lost.clone()));
    assert_eq!(Scratch::rev(&s.checkout(), name), None);
    assert_eq!(done.pruned.len(), 1, "{done:#?}");
    assert_eq!(done.pruned[0].pin.as_deref(), Some(pin.as_str()));
    assert_eq!(
        rows_of(&rows, &pin),
        vec![row(WORKTREE_PUSH_ROW_KIND, "pushed")]
    );
    assert!(pins::load(&s.owed()).expect("the owed file").is_empty());

    // A failed push: the pin stays here, owed, and the name still goes.
    let offline = s.commit("offline");
    s.on_server("agent/offline", &offline);
    s.deleted_on_server("agent/offline");
    let offline_pin = format!("refs/orphans/agent/offline/{}", &offline[..12]);
    let shut = s.origin().join("refs").join("orphans").join("agent");
    std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o555)).expect("shut");
    let (done, rows) = run(&s, &Setup::default());
    std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o755)).expect("open");
    assert!(done.pushed.is_empty(), "{done:#?}");
    assert_eq!(done.owed.len(), 1, "{done:#?}");
    assert!(!done.owed[0].why.is_empty(), "{:?}", done.owed[0]);
    assert_eq!(
        Scratch::rev(&s.checkout(), &offline_pin),
        Some(offline.clone())
    );
    assert_eq!(Scratch::rev(&s.origin(), &offline_pin), None);
    assert_eq!(
        rows_of(&rows, &offline_pin),
        vec![row(WORKTREE_PUSH_ROW_KIND, "owed")]
    );
    assert_eq!(
        Scratch::rev(&s.checkout(), "refs/remotes/origin/agent/offline"),
        None
    );
    let recorded = pins::load(&s.owed()).expect("the owed file");
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    assert_eq!(
        recorded[0].push.refs,
        vec![[offline_pin.clone(), offline.clone()]]
    );
    let no_agh = |_: &[&str]| -> Result<std::process::Output, String> {
        Err("no agh runs in a test".to_owned())
    };
    let later = pins::epoch_secs(SystemTime::now()) + 2 * 60 * 60;
    let round = pins::push_owed(
        &GitCli,
        &no_agh,
        &s.owed(),
        later,
        Instant::now() + MUST_HAPPEN,
    );
    assert_eq!((round.pushed.len(), round.owed.len()), (1, 0), "{round:#?}");
    assert_eq!(Scratch::rev(&s.origin(), &offline_pin), Some(offline));

    // A push the origin took but whose ref reads back there as another commit is not pushed:
    // it stays owed.
    let misread = s.commit("misread");
    s.on_server("agent/misread", &misread);
    s.deleted_on_server("agent/misread");
    let misread_pin = format!("refs/orphans/agent/misread/{}", &misread[..12]);
    let (done, rows) = run(
        &s,
        &Setup {
            misread: true,
            ..Setup::default()
        },
    );
    assert!(done.pushed.is_empty(), "{done:#?}");
    assert_eq!(done.owed.len(), 1, "{done:#?}");
    assert!(done.owed[0].why.contains("read back"), "{:?}", done.owed[0]);
    assert_eq!(
        rows_of(&rows, &misread_pin),
        vec![row(WORKTREE_PUSH_ROW_KIND, "owed")]
    );
    let recorded = pins::load(&s.owed()).expect("the owed file");
    assert_eq!(
        recorded
            .iter()
            .map(|o| o.push.refs.clone())
            .collect::<Vec<_>>(),
        vec![vec![[misread_pin, misread]]]
    );
    assert_eq!(recorded[0].tries, 1, "{recorded:?}");

    // The control: the origin holds the pin's name at another commit.
    let taken = s.commit("taken");
    s.on_server("agent/taken", &taken);
    s.deleted_on_server("agent/taken");
    let taken_pin = format!("refs/orphans/agent/taken/{}", &taken[..12]);
    let main = out(&s.checkout(), &["rev-parse", "main"]);
    git(&s.origin(), &["update-ref", &taken_pin, &main]);
    let (done, rows) = run(&s, &Setup::default());
    assert!(done.pushed.is_empty(), "{done:#?}");
    assert_eq!(done.parked.len(), 1, "{done:#?}");
    assert_eq!(
        rows_of(&rows, &taken_pin),
        vec![row(WORKTREE_PUSH_ROW_KIND, "parked")]
    );
    assert_eq!(Scratch::rev(&s.origin(), &taken_pin), Some(main));
    assert_eq!(Scratch::rev(&s.checkout(), &taken_pin), Some(taken));
}

/// A MERGED BRANCH NO WORKTREE HOLDS IS DELETED, AND AN UNMERGED ONE STAYS: of local branches
/// the server never had, `agent/done` (merged, no worktree) is deleted with its `pruning` row
/// first; `agent/open` (unmerged, no worktree) stays and is pushed under its own name;
/// `agent/held` (merged, a worktree holds it), `agent/used` (merged, a live session used it)
/// and `main` stay. NEGATIVE CONTROLS: while the branches live sessions used are not known, no
/// local branch is deleted or pushed; within [`super::QUIET`] of its last move none is either,
/// whatever program moved it; a ledger that takes no `pruning` row leaves `agent/done`.
#[test]
fn a_merged_branch_no_worktree_holds_is_deleted_and_an_unmerged_one_stays() {
    let s = scratch();
    let merged = s.commit("merged");
    s.advance_main(&merged);
    let open = s.commit("open");
    for (name, oid) in [
        ("agent/done", &merged),
        ("agent/held", &merged),
        ("agent/used", &merged),
        ("agent/open", &open),
    ] {
        git(&s.checkout(), &["branch", name, oid]);
    }
    let held = s.home.join("Developer/.ubd/aterm-held.noindex");
    git(
        &s.checkout(),
        &[
            "worktree",
            "add",
            "-q",
            &held.to_string_lossy(),
            "agent/held",
        ],
    );
    let all = [
        "agent/done",
        "agent/held",
        "agent/used",
        "agent/open",
        "main",
    ];
    let local = |b: &str| Scratch::rev(&s.checkout(), &format!("refs/heads/{b}"));

    let used = || Some(vec!["agent/used".to_owned()]);
    let nothing_moved = |done: &Done, rows: &[String]| {
        assert!(
            done.pruned.is_empty() && done.pushed.is_empty(),
            "{done:#?}"
        );
        assert!(rows.is_empty(), "{rows:?}");
        assert!(all.iter().all(|b| local(b).is_some()));
        assert_eq!(Scratch::rev(&s.origin(), "refs/heads/agent/open"), None);
    };

    // Not known which branches live sessions used: none is judged, a day later or not.
    let (done, rows) = run(
        &s,
        &Setup {
            later: TWO_DAYS,
            ..Setup::default()
        },
    );
    nothing_moved(&done, &rows);

    // Every branch here was made within the day: someone's, whichever program made it.
    let (done, rows) = run(
        &s,
        &Setup {
            used: used(),
            ..Setup::default()
        },
    );
    nothing_moved(&done, &rows);

    // A ledger that takes no `pruning` row: `agent/done` stays (`agent/open` is marked used
    // here, so it is not pushed yet).
    let (done, _) = run(
        &s,
        &Setup {
            used: Some(vec!["agent/used".to_owned(), "agent/open".to_owned()]),
            refuse: Some("pruning"),
            later: TWO_DAYS,
            ..Setup::default()
        },
    );
    assert!(
        done.kept
            .iter()
            .any(|(_, n, _)| n == "refs/heads/agent/done"),
        "kept for want of its row: {done:#?}"
    );
    assert!(
        done.pruned.is_empty() && done.pushed.is_empty(),
        "{done:#?}"
    );
    assert!(all.iter().all(|b| local(b).is_some()));

    let (done, rows) = run(
        &s,
        &Setup {
            used: used(),
            later: TWO_DAYS,
            ..Setup::default()
        },
    );
    assert_eq!(local("agent/done"), None, "{done:#?}");
    assert_eq!(
        rows_of(&rows, "refs/heads/agent/done"),
        vec![row(REF_ROW_KIND, "pruning"), row(REF_ROW_KIND, "pruned")]
    );
    assert_eq!(local("agent/open"), Some(open.clone()));
    assert_eq!(
        Scratch::rev(&s.origin(), "refs/heads/agent/open"),
        Some(open)
    );
    assert_eq!(done.pushed.len(), 1, "{done:#?}");
    for b in ["agent/held", "agent/used"] {
        assert_eq!(local(b), Some(merged.clone()), "{b}");
        assert!(
            rows_of(&rows, &format!("refs/heads/{b}")).is_empty(),
            "{rows:?}"
        );
    }
    assert_eq!(local("main"), Some(merged));
    assert_eq!(done.pruned.len(), 1, "{done:#?}");
}

/// A WORKTREE REGISTRATION WHOSE FOLDER IS GONE (DC-13b) is dropped once the commit only it
/// held is pinned under `refs/orphans/` and that pin's push is on record and sent: its
/// `pinning` row, then its `pruning` row while git's entry stands, then `pruned`. NEGATIVE
/// CONTROLS: while the owed file cannot be read the pin is made but the entry stays, with one
/// `kept` row, and the pin is not sent; the next run finds that pin (its own, which holds
/// nothing for it) and records and sends its push before the entry goes; a worktree whose folder
/// stands is not touched.
#[test]
fn a_gone_worktree_registration_is_dropped_once_its_pins_push_is_on_record() {
    let s = scratch();
    let checkout = s.checkout();
    let add = |name: &str| {
        let wt = s.home.join("wts").join(name);
        git(
            &checkout,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                &wt.to_string_lossy(),
                "HEAD",
            ],
        );
        wt
    };
    let live = add("live.noindex");
    let wt = add("gone.noindex");
    std::fs::write(wt.join("work.txt"), "only this worktree held it\n").expect("written");
    git(&wt, &["add", "work.txt"]);
    git(&wt, &["commit", "-qm", "work only this worktree held"]);
    let head = out(&wt, &["rev-parse", "HEAD"]);
    let entry = checkout.join(".git/worktrees/gone.noindex");
    let name = "worktrees/gone.noindex";
    let pin = format!("refs/orphans/gone/{}", &head[..12]);
    std::fs::remove_dir_all(&wt).expect("removed by hand");

    std::fs::create_dir_all(s.owed().parent().expect("a folder")).expect("made");
    std::fs::write(s.owed(), "not what it writes").expect("written");
    let (done, rows) = run(&s, &Setup::default());
    std::fs::remove_file(s.owed()).expect("removed");
    assert!(done.entries_dropped.is_empty(), "{done:#?}");
    assert_eq!(done.entries_kept.len(), 1, "{done:#?}");
    assert!(
        entry.is_dir(),
        "its entry stays while its pin's push is on no record"
    );
    assert_eq!(Scratch::rev(&checkout, &pin), Some(head.clone()));
    assert_eq!(Scratch::rev(&s.origin(), &pin), None);
    assert_eq!(
        rows_of(&rows, name),
        vec![row(REF_ROW_KIND, "pinning"), row(REF_ROW_KIND, "kept")]
    );

    let (done, rows) = run(&s, &Setup::default());
    assert!(done.entries_kept.is_empty(), "{done:#?}");
    assert_eq!(done.entries_dropped.len(), 1, "{done:#?}");
    let (_, gone, pins_of) = &done.entries_dropped[0];
    assert_eq!(gone.folder, wt);
    assert_eq!(
        pins_of.iter().map(|p| p.name.clone()).collect::<Vec<_>>(),
        vec![pin.clone()]
    );
    assert!(!entry.exists(), "its entry went");
    assert!(live.join(".git").is_file(), "the live worktree stands");
    assert!(checkout.join(".git/worktrees/live.noindex").is_dir());
    assert_eq!(Scratch::rev(&s.origin(), &pin), Some(head.clone()));
    assert_eq!(
        rows_of(&rows, name),
        vec![
            row(REF_ROW_KIND, "pinning"),
            row(REF_ROW_KIND, "pruning"),
            row(REF_ROW_KIND, "pruned"),
        ]
    );
    assert_eq!(
        rows_of(&rows, &pin),
        vec![row(WORKTREE_PUSH_ROW_KIND, "pushed")]
    );
    assert!(pins::load(&s.owed()).expect("the owed file").is_empty());

    let (done, rows) = run(&s, &Setup::default());
    assert!(
        done.entries_dropped.is_empty() && done.entries_kept.is_empty(),
        "{done:#?}"
    );
    assert!(rows_of(&rows, name).is_empty(), "{rows:?}");
}
