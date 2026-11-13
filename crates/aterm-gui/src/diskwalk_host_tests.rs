// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the Disk scopes (flow record §3, step F2a): where each pane is, what it holds, how
//! a scope's bytes nest, the paces and the glance budget, the walker thread, and the build
//! folder hazards. Every tree is a scratch tree under a scratch home.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use aterm_diskwalk::{Deny, HugeDir, Policy, Refusal, Sums, canonical};
use aterm_sysprobe::volumes::{Mount, Table, VolumeKey};

use super::scopes::{
    Clamped, GLANCE_BUDGET, GLANCE_WINDOW, LEAVE_AFTER, PANE_STALE_AFTER, PROMOTE_AFTER,
};
use super::*;

/// The class the end-to-end walker test runs the walker thread in: a CPU
/// class the scheduler keeps running and disk reads its QoS class decides,
/// so its hang detector waits on the walker's own work, not on the
/// product's background class or its throttled reads ([`WALKER_CLASS`]),
/// either of which a saturated machine may hold back before the walker's
/// first publication.
const TEST_WALKER_CLASS: WalkerClass = WalkerClass {
    role: crate::qos::Role::Responsive,
    disk: aterm_diskwalk::DiskIo::Inherited,
};

fn write(path: &Path, bytes: usize) {
    std::fs::write(path, vec![5u8; bytes]).expect("write scratch file");
}

fn mkdir(path: &Path) {
    std::fs::create_dir_all(path).expect("mkdir");
}

fn base(tmp: &aterm_tempfile::TempDir) -> PathBuf {
    canonical(tmp.path()).expect("absolute temp folder")
}

fn policy_for(home: &Path) -> Policy {
    Policy::new(
        Deny::for_home(home, &[], &[]),
        aterm_sysprobe::volumes::table(),
    )
}

fn tab_id(n: u64) -> TabId {
    TabId::from_stored(n)
}

fn set(paths: &[&Path]) -> BTreeSet<PathBuf> {
    paths.iter().map(|p| p.to_path_buf()).collect()
}

// ---------------------------------------------------------------------------------------------
// Where a pane is.

fn pane(key: u64, shell: u32, front: u32, report: Option<CwdReport>) -> PaneFacts {
    PaneFacts {
        key,
        shell_pid: shell,
        front_pid: front,
        report,
    }
}

fn report(path: &str, here: bool) -> Option<CwdReport> {
    Some(CwdReport {
        path: PathBuf::from(path),
        here,
        host: (!here).then(|| "build-host".to_owned()),
    })
}

#[test]
fn a_panes_live_directory_is_its_front_programs_folder_else_its_shells() {
    let cwds = |pid: u32| match pid {
        10 => Some(PathBuf::from("/w/shell")),
        11 => Some(PathBuf::from("/w/front")),
        _ => None,
    };
    let at = |l: Live| match l {
        Live::At { path, from, pid } => (path, from, pid),
        other => panic!("not a local directory: {other:?}"),
    };
    assert_eq!(
        at(live_dir(&pane(1, 10, 11, None), &cwds)),
        (PathBuf::from("/w/front"), LiveFrom::Fg, 11),
        "the program in front"
    );
    assert_eq!(
        at(live_dir(&pane(1, 10, 12, None), &cwds)),
        (PathBuf::from("/w/shell"), LiveFrom::Shell, 10),
        "the shell's when the program in front cannot be read"
    );
    assert_eq!(
        at(live_dir(&pane(1, 10, 10, None), &cwds)),
        (PathBuf::from("/w/shell"), LiveFrom::Shell, 10)
    );
    // Negative control: an OSC 7 report naming another folder while the kernel cwd reads
    // leaves the directory at the kernel's.
    assert_eq!(
        at(live_dir(&pane(1, 10, 11, report("/w/typed", true)), &cwds)),
        (PathBuf::from("/w/front"), LiveFrom::Fg, 11)
    );
    assert_eq!(
        at(live_dir(&pane(1, 20, 20, report("/w/typed", true)), &cwds)),
        (PathBuf::from("/w/typed"), LiveFrom::Osc7, 20),
        "the report only where no kernel cwd reads"
    );
    assert_eq!(live_dir(&pane(1, 20, 21, None), &cwds), Live::Unknown);
}

#[test]
fn a_remote_osc7_report_counts_nothing_on_this_mac() {
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let b = base(&tmp);
    let home = b.join("home");
    let w = home.join("w");
    mkdir(&w.join(".git"));
    let remote = report(&w.display().to_string(), false);
    // ssh in front, its report naming a folder this Mac happens to hold too.
    let ssh = pane(1, 10, 11, remote.clone());
    assert_eq!(
        live_dir(&ssh, &|_| Some(w.clone())),
        Live::Elsewhere {
            host: Some("build-host".to_owned())
        }
    );
    let mut r = Rig::new(&home);
    let seen = r.see(&ssh);
    assert_eq!(seen.view.why, Some(PaneWhy::Elsewhere));
    assert!(seen.named.is_empty(), "{:?}", seen.named);
    // Negative control: the same report made on this machine (the rule the old facts applied
    // to every report) claims the folder.
    let local = pane(1, 10, 11, report(&w.display().to_string(), true));
    let seen = r.see(&local);
    assert_eq!(seen.named.first().map(|c| c.path.clone()), Some(w));
}

#[test]
fn a_remote_report_is_dropped_once_the_shell_is_back_in_front() {
    let back = pane(1, 10, 10, report("/far/away", false));
    assert_eq!(
        live_dir(&back, &|pid| (pid == 10).then(|| PathBuf::from("/w"))),
        Live::At {
            path: PathBuf::from("/w"),
            from: LiveFrom::Shell,
            pid: 10
        }
    );
    assert_eq!(
        live_dir(&back, &|_| None),
        Live::Unknown,
        "never the remote path, even with no kernel cwd"
    );
}

// ---------------------------------------------------------------------------------------------
// Membership.

#[test]
fn a_new_pane_counts_its_folder_at_its_first_discovery() {
    let t0 = Instant::now();
    let p = Path::new("/w");
    let mut m = Membership::default();
    m.observe(&set(&[p]), 1, t0);
    assert!(m.holds(p), "joined at once");
    let mut stale = m.clone();
    let q = Path::new("/q");
    stale.observe(&set(&[q]), 2, t0 + PANE_STALE_AFTER);
    assert!(
        stale.holds(q) && !stale.holds(p),
        "a pane unseen for {PANE_STALE_AFTER:?} starts again"
    );
}

#[test]
fn a_project_joins_at_its_second_discovery_and_leaves_at_its_second_miss() {
    assert_eq!((PROMOTE_AFTER, LEAVE_AFTER), (2, 2));
    let t0 = Instant::now();
    let at = |s: u64| t0 + Duration::from_secs(s);
    let (a, b) = (Path::new("/a"), Path::new("/b"));
    let mut m = Membership::default();
    m.observe(&set(&[a]), 1, at(0));
    m.observe(&set(&[a, b]), 2, at(5));
    assert!(m.joining(b) && !m.holds(b), "named once");
    m.observe(&set(&[a, b]), 3, at(10));
    assert!(m.holds(b), "named twice in a row");
    m.observe(&set(&[b]), 4, at(15));
    assert!(m.holds(a), "one miss keeps it");
    m.observe(&set(&[b]), 5, at(20));
    assert!(!m.holds(a), "two misses in a row");
    m.observe(&set(&[a, b]), 6, at(25));
    m.observe(&set(&[b]), 7, at(30));
    m.observe(&set(&[a, b]), 8, at(35));
    assert!(!m.holds(a), "a miss restarts the count");
}

#[test]
fn a_cd_shorter_than_one_discovery_never_moves_the_tab_segment() {
    let t0 = Instant::now();
    let at = |s: u64| t0 + Duration::from_secs(s);
    let (a, b) = (Path::new("/a"), Path::new("/b"));
    let mut m = Membership::default();
    m.observe(&set(&[a]), 1, at(0));
    let before: Vec<PathBuf> = m.members().cloned().collect();
    // A cd to /b and back that one discovery saw, and one none saw.
    m.observe(&set(&[b]), 2, at(5));
    m.observe(&set(&[a]), 3, at(10));
    m.observe(&set(&[a]), 4, at(15));
    assert_eq!(m.members().cloned().collect::<Vec<_>>(), before);
    assert!(!m.joining(b));
}

// ---------------------------------------------------------------------------------------------
// The engine, driven directly with injected process reads.

/// An engine whose process reads are a table the test fills, counted.
struct Rig {
    engine: Engine,
    _to: Sender<ToWalker>,
    cwd: Rc<RefCell<BTreeMap<u32, PathBuf>>>,
    cwd_reads: Rc<Cell<usize>>,
    exe_reads: Rc<Cell<usize>>,
    other_pool: Rc<RefCell<Option<PathBuf>>>,
}

impl Rig {
    fn new(home: &Path) -> Self {
        let (to, inbox) = std::sync::mpsc::channel();
        let mut engine = Engine::new(inbox, Arc::default(), None);
        let cwd: Rc<RefCell<BTreeMap<u32, PathBuf>>> = Rc::default();
        let cwd_reads: Rc<Cell<usize>> = Rc::default();
        let exe_reads: Rc<Cell<usize>> = Rc::default();
        let other_pool: Rc<RefCell<Option<PathBuf>>> = Rc::default();
        engine.reads = PaneReads {
            cwd: Box::new({
                let (cwd, n) = (Rc::clone(&cwd), Rc::clone(&cwd_reads));
                move |pid| {
                    n.set(n.get() + 1);
                    cwd.borrow().get(&pid).cloned()
                }
            }),
            exe: Box::new({
                let n = Rc::clone(&exe_reads);
                move |pid| {
                    n.set(n.get() + 1);
                    Some(format!("p{pid}"))
                }
            }),
            pool: Box::new({
                let other = Rc::clone(&other_pool);
                move |path, _| {
                    let elsewhere = other.borrow().as_ref().is_some_and(|o| path.starts_with(o));
                    Some(VolumeKey::Fsid(if elsewhere { 2 } else { 1 }))
                }
            }),
        };
        engine.take_facts(Facts {
            home: Some(home.to_path_buf()),
            ..Facts::default()
        });
        engine.home_pool = Some(VolumeKey::Fsid(1));
        Self {
            engine,
            _to: to,
            cwd,
            cwd_reads,
            exe_reads,
            other_pool,
        }
    }

    fn see(&mut self, p: &PaneFacts) -> Seen {
        let policy = self.engine.policy.clone().expect("a policy");
        let home = self.engine.facts.home.clone();
        self.engine
            .see_pane(p, &policy, home.as_deref(), Instant::now())
    }

    /// Panes `(key, tab, cwd)`, each shell its key's pid, in tabs numbered from 1.
    fn facts(&mut self, panes: &[(u64, u64, &Path)], own: &[PathBuf], opt_in: &[PathBuf]) {
        let mut tabs: Vec<TabFacts> = Vec::new();
        let mut facts = Vec::new();
        for &(key, tab, cwd) in panes {
            let pid = u32::try_from(key).expect("small key") + 100;
            self.cwd.borrow_mut().insert(pid, cwd.to_path_buf());
            facts.push(pane(key, pid, pid, None));
            match tabs.iter_mut().find(|t| t.id == tab_id(tab)) {
                Some(t) => t.panes.push(key),
                None => tabs.push(TabFacts {
                    id: tab_id(tab),
                    label: format!("Tab {tab}"),
                    number: usize::try_from(tab).ok(),
                    window: 1,
                    panes: vec![key],
                }),
            }
        }
        let home = self.engine.facts.home.clone();
        self.engine.take_facts(Facts {
            panes: facts,
            tabs,
            home,
            overrides: Vec::new(),
            opt_in: opt_in.to_vec(),
            own: own.to_vec(),
        });
    }

    fn cd(&mut self, key: u64, to: &Path) {
        let pid = u32::try_from(key).expect("small key") + 100;
        self.cwd.borrow_mut().insert(pid, to.to_path_buf());
    }

    fn demand(&mut self, scope: DiskScope, pace: Pace, at: Instant) {
        self.engine.apply(ToWalker::Demand {
            scope,
            detail: Detail::Sizes,
            pace,
            at,
        });
    }

    /// Walk every root owed at `now` to its end.
    fn settle(&mut self, now: Instant) -> Vec<PathBuf> {
        let mut walked = Vec::new();
        for _ in 0..64 {
            if self.engine.active.is_some() {
                self.engine.step_walk();
            } else if let Some(p) = self.engine.next_root(now) {
                walked.push(p.clone());
                self.engine.start_walk(&p);
            } else {
                return walked;
            }
        }
        panic!("the walks never settled: {walked:?}");
    }

    fn board(&self) -> Published {
        self.engine.published()
    }
}

#[test]
fn a_live_directory_lifts_to_the_repository_git_names() {
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let b = base(&tmp);
    let home = b.join("home");
    let repo = home.join("src/repo");
    for d in [
        "src/repo/.git",
        "src/repo/crates/x",
        "src/repo/vendor/sub/src",
        "src/linked/src",
        "src/plain/deep",
    ] {
        mkdir(&home.join(d));
    }
    write(&repo.join("vendor/sub/.git"), 30);
    write(&home.join("src/linked/.git"), 30);
    let policy = policy_for(&home);
    let l = lift(&repo.join("crates/x"), Some(&home), &policy);
    assert_eq!(
        (l.project.clone(), l.kind, l.linked),
        (repo.clone(), RootKind::Project, false),
        "a subfolder"
    );
    let l = lift(&repo.join("vendor/sub/src"), Some(&home), &policy);
    assert_eq!(
        (l.project.clone(), l.linked),
        (repo.join("vendor/sub"), true),
        "a submodule is its own repository"
    );
    let l = lift(&home.join("src/linked/src"), Some(&home), &policy);
    assert_eq!(
        (l.project.clone(), l.kind, l.linked),
        (home.join("src/linked"), RootKind::Project, true),
        "a linked worktree"
    );
    let l = lift(&home.join("src/plain/deep"), Some(&home), &policy);
    assert_eq!(
        (l.project.clone(), l.kind),
        (home.join("src/plain/deep"), RootKind::Folder),
        "no marker: the live directory itself"
    );
}

#[test]
fn a_live_directory_at_home_root_or_a_volume_root_is_named_too_broad_and_counts_nothing() {
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let b = base(&tmp);
    let home = b.join("home");
    mkdir(&home.join("w/.git"));
    let mut r = Rig::new(&home);
    for (key, at) in [(1, home.clone()), (2, PathBuf::from("/"))] {
        let pid = 100 + u32::try_from(key).expect("small");
        r.cwd.borrow_mut().insert(pid, at.clone());
        let seen = r.see(&pane(key, pid, pid, None));
        assert_eq!(
            seen.view.why,
            Some(PaneWhy::Refused(Refusal::TooBroad)),
            "{}",
            at.display()
        );
        assert!(seen.named.is_empty());
        assert_eq!(seen.view.live.as_deref(), Some(at.as_path()), "named");
    }
}

#[test]
fn a_protected_or_remote_live_directory_is_never_opened_or_lifted() {
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let b = base(&tmp);
    let home = b.join("home");
    let private = home.join("Documents/proj/sub");
    let open = home.join("src/proj/sub");
    mkdir(&private);
    mkdir(&home.join("Documents/proj/.git"));
    mkdir(&open);
    let mut r = Rig::new(&home);
    r.cwd.borrow_mut().insert(101, private.clone());
    r.cwd.borrow_mut().insert(102, open.clone());
    let (seen, touched) = aterm_diskwalk::touched::record(|| r.see(&pane(1, 101, 101, None)));
    assert_eq!(seen.view.why, Some(PaneWhy::Refused(Refusal::Protected)));
    assert_eq!(seen.view.project, None, "not lifted");
    assert!(seen.named.is_empty());
    assert!(
        touched.is_empty(),
        "nothing was opened for a protected pane: {touched:?}"
    );
    // A live directory on a remote mount: refused before anything under it is opened.
    let net = home.join("net");
    let remote = net.join("proj/sub");
    mkdir(&remote);
    mkdir(&net.join("proj/.git"));
    r.engine
        .policy
        .as_mut()
        .expect("a policy")
        .mounts
        .mounts
        .push(Mount {
            on: net.clone(),
            from: "server:/share".into(),
            fstype: "nfs".into(),
            local: false,
            automounted: false,
            removable: false,
            read_only: false,
            container: None,
        });
    r.cwd.borrow_mut().insert(103, remote.clone());
    let (seen, touched) = aterm_diskwalk::touched::record(|| r.see(&pane(3, 103, 103, None)));
    assert_eq!(seen.view.why, Some(PaneWhy::Refused(Refusal::Remote)));
    assert_eq!(seen.view.project, None, "not lifted");
    assert!(seen.named.is_empty());
    assert!(
        touched.is_empty(),
        "nothing was opened for a remote pane: {touched:?}"
    );
    // Negative control: a cwd under ~/src opens its ancestors for `.git`.
    let (seen, touched) = aterm_diskwalk::touched::record(|| r.see(&pane(2, 102, 102, None)));
    assert_eq!(seen.view.why, None);
    assert!(
        touched.iter().any(|p| p.starts_with(home.join("src"))),
        "{touched:?}"
    );
    assert!(
        touched
            .iter()
            .all(|p| !p.starts_with(home.join("Documents"))),
        "{touched:?}"
    );
}

#[test]
fn every_workspaces_build_mirror_under_a_project_is_its_build_output() {
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let b = base(&tmp);
    let home = b.join("home");
    let repo = home.join("src/repo");
    mkdir(&repo.join(".git"));
    mkdir(&repo.join("tools/nested"));
    mkdir(&b.join("out/target"));
    std::os::unix::fs::symlink(b.join("out/target"), repo.join("target")).expect("link");
    let root = atpkg::build_cache::cache_root(&home);
    let top = atpkg::build_cache::build_dir(&root, &repo);
    let nested = atpkg::build_cache::build_dir(&root, &repo.join("tools/nested"));
    mkdir(&top);
    mkdir(&nested);
    let policy = policy_for(&home);
    let l = lift(&repo, Some(&home), &policy);
    let mirror = build_mirror(&root, &repo);
    assert_eq!(l.builds, vec![b.join("out/target"), mirror.clone()]);
    assert!(
        nested.starts_with(&mirror),
        "the mirror holds the nested workspace's"
    );
    // Negative control: the top workspace's build directory alone misses it.
    assert!(!nested.starts_with(&top));
    // A target inside the project is a pinned child, not a build root.
    std::fs::remove_file(repo.join("target")).expect("unlink");
    mkdir(&repo.join("target"));
    assert_eq!(lift(&repo, Some(&home), &policy).builds, vec![mirror]);
}

#[test]
fn discovery_reads_no_process_table_environment_or_open_files() {
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let b = base(&tmp);
    let home = b.join("home");
    let w = home.join("w");
    mkdir(&w.join(".git"));
    let mut r = Rig::new(&home);
    r.cwd.borrow_mut().insert(11, w.clone());
    let reads = |r: &Rig| (r.cwd_reads.get(), r.exe_reads.get());
    r.see(&pane(1, 10, 11, None));
    assert_eq!(reads(&r), (1, 1), "the front program's cwd and name");
    r.see(&pane(2, 11, 12, None));
    assert_eq!(
        reads(&r),
        (3, 2),
        "two cwd reads when the front program's cannot be read"
    );
    // Discovery's only reads of a process are these two injected ones: the engine holds no
    // process table, environment or open-file reader to call (a counting seam, `PaneReads`).
    // Non-vacuity: a discovery of a demanded pane goes through them.
    r.facts(&[(5, 1, &w)], &[], &[]);
    r.demand(DiskScope::Tab(tab_id(1)), Pace::Look, Instant::now());
    let before = reads(&r);
    r.engine.discover_if_due(Instant::now());
    assert_eq!(reads(&r).0, before.0 + 1);
    assert_eq!(reads(&r).1, before.1 + 1);
}

#[test]
fn a_cargo_target_dir_a_scratch_folder_and_a_childs_cwd_are_not_the_tabs() {
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let b = base(&tmp);
    let home = b.join("home");
    let w = home.join("w");
    mkdir(&w.join(".git"));
    let elsewhere = home.join("cargo-target");
    mkdir(&elsewhere);
    let mut r = Rig::new(&home);
    // The shell is at w; a child (pid 999, not in front) works in the target folder.
    r.cwd.borrow_mut().insert(999, elsewhere.clone());
    r.facts(&[(1, 1, &w)], &[], &[]);
    let t0 = Instant::now();
    r.demand(DiskScope::Tab(tab_id(1)), Pace::Look, t0);
    r.engine.discover_if_due(t0);
    let board = r.board();
    let members: Vec<PathBuf> = board
        .members(DiskScope::Tab(tab_id(1)))
        .iter()
        .map(|c| c.path.clone())
        .collect();
    assert!(members.contains(&w));
    assert!(
        !members.iter().any(|m| m.starts_with(&elsewhere)),
        "{members:?}"
    );
}

#[test]
fn a_tab_is_the_union_of_its_panes_projects() {
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let b = base(&tmp);
    let home = b.join("home");
    let (one, two) = (home.join("one"), home.join("two"));
    mkdir(&one.join(".git"));
    mkdir(&two.join("src"));
    let mut r = Rig::new(&home);
    r.facts(&[(1, 1, &one), (2, 1, &two.join("src"))], &[], &[]);
    let t0 = Instant::now();
    r.demand(DiskScope::Tab(tab_id(1)), Pace::Look, t0);
    r.engine.discover_if_due(t0);
    let members = |r: &Rig| -> BTreeSet<PathBuf> {
        r.board()
            .members(DiskScope::Tab(tab_id(1)))
            .iter()
            .map(|c| c.path.clone())
            .collect()
    };
    assert_eq!(members(&r), set(&[&one, &two.join("src")]));
    // Negative control: facts of the focused pane alone miss the second pane.
    let mut focus = Rig::new(&home);
    focus.facts(&[(1, 1, &one)], &[], &[]);
    focus.demand(DiskScope::Tab(tab_id(1)), Pace::Look, t0);
    focus.engine.discover_if_due(t0);
    assert_eq!(members(&focus), set(&[&one]));
}

/// A FIRST ASK IS OWED, NOT ZERO (main's d8a5126907, on F2a's engine): a discovery covers the
/// scopes demanded when it ran, so a known tab no discovery has derived reads `undiscovered` and
/// `walking`, never "0 roots, 0 B"; so does aterm until a discovery covered it, and every scope
/// once a pane opened or closed since, until the prompt discovery of the new shape. Negative
/// control: the scope the discovery covered reads discovered, with its roots.
#[test]
fn a_scope_no_discovery_has_derived_on_these_facts_is_owed_not_empty() {
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let b = base(&tmp);
    let home = b.join("home");
    let (w, v) = (home.join("w"), home.join("v"));
    mkdir(&w.join(".git"));
    mkdir(&v.join(".git"));
    let report = |p: &Published, scope| build_report(scope, p, WalkerState::Running);
    assert!(
        report(&Published::default(), DiskScope::Aterm).undiscovered,
        "an empty board has derived nothing"
    );
    let (one, two) = (DiskScope::Tab(tab_id(1)), DiskScope::Tab(tab_id(2)));
    let mut r = Rig::new(&home);
    r.facts(&[(1, 1, &w), (2, 2, &v)], &[], &[]);
    let now = Instant::now();
    r.demand(one, Pace::Look, now);
    r.engine.discover_if_due(now);
    let p = r.board();
    let t1 = report(&p, one);
    assert!(!t1.undiscovered && !t1.roots.is_empty(), "{t1:#?}");
    let t2 = report(&p, two);
    assert_eq!(t2.label.as_deref(), Some("Tab 2"), "the facts name it");
    assert!(!t2.unknown_tab);
    assert!(
        t2.undiscovered && t2.walking && t2.roots.is_empty(),
        "a known tab with no derived roots is owed, not empty: {t2:#?}"
    );
    let all = report(&p, DiskScope::Aterm);
    assert!(all.undiscovered && all.walking, "{all:#?}");
    assert!(!all.roots.is_empty(), "what is derived still shows");
    // A pane opens in Tab 1: the discovery of the older shape covers none of these panes.
    r.facts(&[(1, 1, &w), (2, 2, &v), (3, 1, &v)], &[], &[]);
    assert!(report(&r.board(), one).undiscovered, "a new shape is owed");
    r.engine.discover_if_due(now);
    let t1 = report(&r.board(), one);
    assert!(!t1.undiscovered, "the prompt discovery covers it: {t1:#?}");
    assert!(
        report(&r.board(), two).undiscovered,
        "Tab 2 is still not asked"
    );
    r.demand(DiskScope::Aterm, Pace::Look, now);
    r.engine.discover_if_due(now);
    for scope in [one, two, DiskScope::Aterm] {
        assert!(!report(&r.board(), scope).undiscovered, "{scope:?} derived");
    }
}

/// A focus moved between splits, or a tab moved along the strip, opens and closes no pane: the
/// same panes in another order are no shape change, so the discovery the board carries stands
/// and no prompt round runs. Negative control: a pane that opens is one.
#[test]
fn the_same_panes_in_another_order_are_no_shape_change() {
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let b = base(&tmp);
    let home = b.join("home");
    let (w, v) = (home.join("w"), home.join("v"));
    mkdir(&w.join(".git"));
    mkdir(&v.join(".git"));
    let one = DiskScope::Tab(tab_id(1));
    let mut r = Rig::new(&home);
    r.facts(&[(1, 1, &w), (3, 1, &v), (2, 2, &v)], &[], &[]);
    let now = Instant::now();
    r.demand(one, Pace::Look, now);
    r.engine.discover_if_due(now);
    let seq = r.engine.shape_seq;
    r.facts(&[(2, 2, &v), (3, 1, &v), (1, 1, &w)], &[], &[]);
    assert_eq!(
        r.engine
            .facts
            .tabs
            .iter()
            .map(|t| t.panes.clone())
            .collect::<Vec<_>>(),
        [vec![2], vec![3, 1]],
        "the facts did move"
    );
    assert_eq!(r.engine.shape_seq, seq, "the same panes in the same tabs");
    let t1 = build_report(one, &r.board(), WalkerState::Running);
    assert!(!t1.undiscovered && !t1.roots.is_empty(), "{t1:#?}");
    r.facts(&[(2, 2, &v), (3, 1, &v), (1, 1, &w), (4, 1, &w)], &[], &[]);
    assert_eq!(r.engine.shape_seq, seq + 1, "a pane opened");
    assert!(build_report(one, &r.board(), WalkerState::Running).undiscovered);
}

#[test]
fn a_root_left_under_ten_minutes_ago_returns_without_a_walk() {
    assert_eq!(RETIRE_AFTER, Duration::from_secs(600));
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let b = base(&tmp);
    let home = b.join("home");
    let (w, x) = (home.join("w"), home.join("x"));
    mkdir(&w.join(".git"));
    mkdir(&x.join(".git"));
    write(&w.join("f"), 9_000);
    let mut r = Rig::new(&home);
    r.facts(&[(1, 1, &w)], &[], &[]);
    let tab = DiskScope::Tab(tab_id(1));
    let t0 = Instant::now();
    let at = |s: u64| t0 + Duration::from_secs(s);
    let step = |r: &mut Rig, s: u64| {
        r.demand(tab, Pace::Glance, at(s));
        r.engine.discover_if_due(at(s));
    };
    step(&mut r, 0);
    assert_eq!(r.settle(at(0)), vec![w.clone()]);
    r.cd(1, &x);
    for s in [5, 10, 15] {
        step(&mut r, s);
    }
    assert!(!r.board().members(tab).iter().any(|c| c.path == w), "left");
    r.settle(at(15));
    for s in [70, 295] {
        step(&mut r, s);
        assert!(
            r.engine.records.contains_key(&w),
            "kept {s} s on (a 60 s retirement would have dropped it)"
        );
        assert!(
            !r.engine.plan(at(s)).contains(&w),
            "and not walked while it is out of every scope"
        );
    }
    r.cd(1, &w);
    step(&mut r, 300);
    step(&mut r, 305);
    assert!(r.board().members(tab).iter().any(|c| c.path == w), "back");
    let fig = r.board().figure(tab);
    assert!(
        fig.drawn() && fig.counted >= 1,
        "draws again before any walk: {fig:?}"
    );
    assert!(
        !r.settle(at(305)).contains(&w),
        "with no walk: its last one stands"
    );
    // 600 s after it leaves again, its record goes, and it draws only once walked.
    r.cd(1, &x);
    for s in [310, 315, 320] {
        step(&mut r, s);
    }
    step(&mut r, 930);
    assert!(!r.engine.records.contains_key(&w));
}

#[test]
fn a_periodic_discovery_that_finds_nothing_new_stores_and_wakes_nothing() {
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let b = base(&tmp);
    let home = b.join("home");
    let (w, x) = (home.join("w"), home.join("x"));
    mkdir(&w.join(".git"));
    mkdir(&x.join(".git"));
    write(&w.join("f"), 9_000);
    let mut r = Rig::new(&home);
    r.facts(&[(1, 1, &w)], &[], &[]);
    let tab = DiskScope::Tab(tab_id(1));
    let t0 = Instant::now();
    let at = |s: u64| t0 + Duration::from_secs(s);
    let round = |r: &mut Rig, s: u64| {
        r.demand(tab, Pace::Glance, at(s));
        r.engine.discover_if_due(at(s));
        r.engine.publish_if_dirty();
        r.engine.out.take()
    };
    assert!(round(&mut r, 0).is_some(), "the first discovery publishes");
    r.settle(at(0));
    r.engine.publish_if_dirty();
    let walked = r.engine.out.take().expect("the walk publishes");
    assert!(walked.records.contains_key(&w));
    for s in [5, 10, 15] {
        assert!(
            round(&mut r, s).is_none(),
            "a discovery {s} s on that finds the same panes stores nothing"
        );
    }
    r.cd(1, &x);
    let moved = round(&mut r, 20).expect("a cd the discovery sees publishes at once");
    assert!(
        moved
            .panes
            .get(&1)
            .is_some_and(|v| v.project.as_ref() == Some(&x)),
        "the pane's new project, joining: {:?}",
        moved.panes.get(&1)
    );
    let joined = round(&mut r, 25).expect("its join publishes");
    assert!(joined.members(tab).iter().any(|c| c.path == x), "joined");
    assert_eq!(
        *r.engine.last.as_deref().expect("stored"),
        r.board(),
        "what readers hold is the board now"
    );
}

#[test]
fn the_aterm_scope_is_every_open_tabs_folders_and_atermss_own_each_folder_once() {
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let b = base(&tmp);
    let home = b.join("home");
    let sup = home.join("super");
    let sub = sup.join("vendor/sub");
    mkdir(&sup.join(".git"));
    mkdir(&sub);
    write(&sub.join(".git"), 20);
    write(&sup.join("a"), 8_192);
    write(&sub.join("b"), 4_096);
    let caches = home.join("Library/Caches/aterm");
    let mirror = build_mirror(&atpkg::build_cache::cache_root(&home), &sup);
    mkdir(&mirror.join(".cargo-build/debug"));
    write(&mirror.join(".cargo-build/debug/o"), 4_096);
    let state = home.join("state");
    mkdir(&state);
    let mut r = Rig::new(&home);
    r.facts(
        &[(1, 1, &sup), (2, 2, &sup), (3, 3, &sub)],
        &[state.clone(), caches.clone()],
        &[],
    );
    let t0 = Instant::now();
    r.demand(DiskScope::Aterm, Pace::Look, t0);
    r.engine.discover_if_due(t0);
    r.settle(t0);
    let board = r.board();
    let members: Vec<PathBuf> = board
        .members(DiskScope::Aterm)
        .iter()
        .map(|c| c.path.clone())
        .collect();
    let unique: BTreeSet<&PathBuf> = members.iter().collect();
    assert_eq!(unique.len(), members.len(), "each folder once: {members:?}");
    assert!(members.contains(&sup) && members.contains(&sub) && members.contains(&mirror));
    let report = build_report(DiskScope::Aterm, &board, WalkerState::Running);
    let fig = board.figure(DiskScope::Aterm);
    let walked = |p: &Path| {
        board.records[p]
            .complete
            .as_ref()
            .expect("complete")
            .sums
            .alloc_fair
    };
    assert_eq!(
        fig.bytes,
        walked(&sup) + walked(&caches) + walked(&state),
        "the submodule inside its superproject and the mirror inside aterm's caches count once"
    );
    assert_eq!(report.total_bytes, fig.bytes);
    let sub_row = report
        .roots
        .iter()
        .find(|row| row.claim.path == sub)
        .expect("the submodule's row");
    assert!(sub_row.inside.is_some(), "pinned inside the superproject");
    assert_eq!(sub_row.record.read_from.as_deref(), Some(sup.as_path()));
}

/// A NESTED ROOT IS READ FROM ITS ENCLOSING WALK ONCE PER WALK: a reading scans the enclosing
/// walk's whole tree (the build cache's is millions of folders), so a discovery that finds the
/// same walks keeps the reading it has, and the snapshot check finds each shown row of a walk
/// once. Negative control: a rewalk of the enclosing root reads the nested root again.
#[test]
fn a_nested_root_is_read_from_its_enclosing_walk_once_per_walk() {
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let b = base(&tmp);
    let home = b.join("home");
    let w = home.join("w");
    mkdir(&w.join(".git"));
    write(&w.join("f"), 4_096);
    let caches = home.join("Library/Caches/aterm");
    let mirror = build_mirror(&atpkg::build_cache::cache_root(&home), &w);
    mkdir(&mirror.join(".cargo-build/debug"));
    write(&mirror.join(".cargo-build/debug/o"), 8_192);
    let mut r = Rig::new(&home);
    r.facts(&[(1, 1, &w)], std::slice::from_ref(&caches), &[]);
    let t0 = Instant::now();
    r.demand(DiskScope::Aterm, Pace::Look, t0);
    r.engine.discover_if_due(t0);
    r.settle(t0);
    let read = |r: &Rig| Arc::clone(&r.engine.records[&mirror]);
    let first = read(&r);
    assert_eq!(first.read_from.as_deref(), Some(caches.as_path()));
    let t1 = t0 + DISCOVER_EVERY;
    r.demand(DiskScope::Aterm, Pace::Look, t1);
    r.engine.discover_if_due(t1);
    assert_eq!(r.engine.periodic_at, Some(t1), "a periodic round ran");
    assert!(
        Arc::ptr_eq(&first, &read(&r)),
        "the same walk, the same reading"
    );
    let tree = Arc::clone(&r.engine.kept.trees[&caches]);
    assert!(
        r.engine
            .kept
            .found
            .get(&caches)
            .is_some_and(|(t, rows)| Arc::ptr_eq(t, &tree) && !rows.is_empty()),
        "the shown rows of this walk, found once"
    );
    write(&mirror.join(".cargo-build/debug/p"), 16_384);
    let later = t1 + Duration::from_secs(120);
    r.demand(DiskScope::Aterm, Pace::Look, later);
    r.engine.discover_if_due(later);
    // Each walk owed at `later`, once: a walk ends at the clock's now, before `later`.
    let mut walked = BTreeSet::new();
    while let Some(p) = r.engine.next_root(later) {
        if !walked.insert(p.clone()) {
            break;
        }
        r.engine.start_walk(&p);
        while r.engine.active.is_some() {
            r.engine.step_walk();
        }
    }
    assert!(
        walked.contains(&caches),
        "the look pace rewalks it: {walked:?}"
    );
    let again = read(&r);
    assert!(!Arc::ptr_eq(&first, &again), "a new walk, a new reading");
    assert!(again.alloc() > first.alloc(), "{again:?}");
    assert!(
        r.engine
            .kept
            .found
            .get(&caches)
            .is_none_or(|(t, _)| Arc::ptr_eq(t, &r.engine.kept.trees[&caches])),
        "no rows found in the old walk are kept"
    );
}

#[test]
fn opt_in_roots_are_walked_and_in_no_scope() {
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let b = base(&tmp);
    let home = b.join("home");
    let w = home.join("w");
    let extra = b.join("derived");
    mkdir(&w.join(".git"));
    mkdir(&extra);
    write(&extra.join("x"), 4_096);
    let mut r = Rig::new(&home);
    r.facts(&[(1, 1, &w)], &[], std::slice::from_ref(&extra));
    let t0 = Instant::now();
    r.demand(DiskScope::Aterm, Pace::Look, t0);
    r.demand(DiskScope::Tab(tab_id(1)), Pace::Look, t0);
    r.engine.discover_if_due(t0);
    let walked = r.settle(t0);
    assert!(walked.contains(&extra), "walked: {walked:?}");
    let board = r.board();
    for scope in [DiskScope::Aterm, DiskScope::Tab(tab_id(1))] {
        assert!(!board.members(scope).iter().any(|c| c.path == extra));
    }
    assert!(board.scopes_of(&extra).is_empty());
    let report = build_report(DiskScope::Aterm, &board, WalkerState::Running);
    let row = report
        .roots
        .iter()
        .find(|row| row.claim.path == extra)
        .expect("listed");
    assert_eq!(row.claim.kinds, BTreeSet::from([RootKind::OptIn]));
    assert_eq!(report.total_bytes, board.figure(DiskScope::Aterm).bytes);
    assert_eq!(
        board.figure(DiskScope::Aterm).counted,
        1,
        "only the tab's project counts"
    );
}

#[test]
fn atermss_own_folders_hold_its_state_and_package_prefix_on_every_unix_and_the_build_cache_on_macos()
 {
    let home = Path::new("/Users//a");
    let state = home.join("Library/Application Support/aterm");
    let mac = own_paths(Some(home), Some(state.clone()), true);
    assert_eq!(
        mac,
        vec![
            state.clone(),
            home.join("Library/Caches/aterm"),
            atpkg::platform::default_prefix(home),
        ]
    );
    let linux = own_paths(
        Some(Path::new("/home/a")),
        Some(PathBuf::from("/home/a/.local/state/aterm")),
        false,
    );
    assert_eq!(
        linux,
        vec![
            PathBuf::from("/home/a/.local/state/aterm"),
            atpkg::platform::default_prefix(Path::new("/home/a")),
        ],
        "no build cache folder off macOS"
    );
    // The prefix inside the state folder (macOS's) is that folder's; outside it (Linux's) it
    // is its own.
    let inside = own_folders(
        &[
            state.clone(),
            home.join("Library/Caches/aterm"),
            state.join("pkg"),
        ],
        true,
    );
    assert_eq!(inside, vec![state, home.join("Library/Caches/aterm")]);
    let apart = own_folders(
        &[
            PathBuf::from("/home/a/.local/state/aterm"),
            PathBuf::from("/home/a/.local/share/aterm/pkg"),
        ],
        false,
    );
    assert_eq!(apart.len(), 2);
}

#[test]
fn a_tab_on_another_pool_adds_nothing_to_the_shown_disks_segments() {
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let b = base(&tmp);
    let home = b.join("home");
    let (w, far) = (home.join("w"), home.join("far"));
    mkdir(&w.join(".git"));
    mkdir(&far.join(".git"));
    write(&w.join("f"), 4_096);
    let mut r = Rig::new(&home);
    *r.other_pool.borrow_mut() = Some(far.clone());
    r.facts(&[(1, 1, &w), (2, 2, &far)], &[], &[]);
    let t0 = Instant::now();
    r.demand(DiskScope::Aterm, Pace::Look, t0);
    r.engine.discover_if_due(t0);
    let walked = r.settle(t0);
    assert!(!walked.contains(&far), "not walked: {walked:?}");
    let board = r.board();
    assert_eq!(
        board.panes[&2].why,
        Some(PaneWhy::OtherPool),
        "named, not counted"
    );
    assert!(!board.figure(DiskScope::Tab(tab_id(2))).drawn());
    let fig = board.figure(DiskScope::Aterm);
    assert_eq!(fig.counted, 1);
}

#[test]
fn a_volume_of_the_same_container_counts() {
    let mount = |on: &str, container: &str| Mount {
        on: PathBuf::from(on),
        from: format!("/dev/{container}s1"),
        fstype: "apfs".into(),
        local: true,
        automounted: false,
        removable: false,
        read_only: false,
        container: Some(container.into()),
    };
    let table = Table {
        mounts: vec![
            mount("/", "disk3"),
            mount("/System/Volumes/Data", "disk3"),
            mount("/Volumes/Work", "disk3"),
            mount("/Volumes/Other", "disk5"),
        ],
        complete: true,
        firmlinks: Vec::new(),
    };
    let home = pool_key(Path::new("/System/Volumes/Data/Users//a"), &table);
    assert_eq!(home, Some(VolumeKey::Container("disk3".into())));
    let key = |p: &str| pool_key(Path::new(p), &table);
    assert_eq!(
        Pool::of(key("/Volumes/Work/x").as_ref(), home.as_ref()),
        Pool::Same,
        "another volume of home's container"
    );
    assert_eq!(
        Pool::of(key("/Volumes/Other/x").as_ref(), home.as_ref()),
        Pool::Other
    );
    assert_eq!(Pool::of(None, home.as_ref()), Pool::Unknown);
}

// ---------------------------------------------------------------------------------------------
// Bytes, each folder once, from one board.

fn sums(alloc: u64) -> Sums {
    Sums {
        alloc_fair: alloc,
        logical: alloc,
        files: 3,
        dirs: 1,
        ..Sums::default()
    }
}

fn complete(alloc: u64, huge: Vec<HugeDir>, growth: Option<Growth>) -> Complete {
    Complete {
        sums: sums(alloc),
        detail: Detail::Sizes,
        links: None,
        at: Instant::now(),
        took: Duration::from_millis(5),
        direct_entries_max: 4,
        biggest: Vec::new(),
        most_entries: Vec::new(),
        huge,
        not_measured: Vec::new(),
        growth,
    }
}

fn record(path: &str, state: RootState, c: Option<Complete>) -> RootRecord {
    RootRecord {
        complete: c,
        state,
        ..RootRecord::queued(PathBuf::from(path))
    }
}

fn standing<'a>(path: &'a Path, pool: Pool, record: Option<&'a RootRecord>) -> Standing<'a> {
    Standing { path, pool, record }
}

#[test]
fn a_disk_scope_draws_nothing_before_one_of_its_folders_is_walked() {
    let q = record("/w", RootState::Queued, None);
    let walking = RootRecord {
        partial: Some(sums(9_000)),
        ..record("/w", RootState::Walking, None)
    };
    for r in [None, Some(&q), Some(&walking)] {
        let f = scope_figure(&[standing(Path::new("/w"), Pool::Same, r)], true);
        assert!(!f.drawn(), "{r:?}");
        assert_eq!(f.why_not_drawn(), Some("walking"));
        assert_eq!(f.bytes, 0);
    }
    let refused = record("/w", RootState::NotMeasured(Refusal::Protected), None);
    let f = scope_figure(
        &[standing(Path::new("/w"), Pool::Same, Some(&refused))],
        true,
    );
    assert_eq!(f.why_not_drawn(), Some("none_counted"));
    assert!(f.complete, "nothing walkable is owed");
}

#[test]
fn a_partial_walk_never_widens_a_segment() {
    let walking = RootRecord {
        partial: Some(sums(90_000)),
        ..record(
            "/w",
            RootState::Walking,
            Some(complete(10_000, Vec::new(), None)),
        )
    };
    let f = scope_figure(
        &[standing(Path::new("/w"), Pool::Same, Some(&walking))],
        true,
    );
    assert_eq!(
        f.bytes, 10_000,
        "the prior complete figure, not the partial"
    );
    assert_eq!(walking.alloc(), 10_000);
    // Negative control: the partial the old fallback read is wider.
    assert!(walking.partial.expect("partial").alloc_fair > f.bytes);
}

#[test]
fn a_rewalk_keeps_the_segment_drawn() {
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let b = base(&tmp);
    let home = b.join("home");
    let w = home.join("w");
    mkdir(&w.join(".git"));
    for i in 0..64 {
        write(&w.join(format!("f{i}")), 512);
    }
    let mut r = Rig::new(&home);
    r.facts(&[(1, 1, &w)], &[], &[]);
    let tab = DiskScope::Tab(tab_id(1));
    let t0 = Instant::now();
    r.demand(tab, Pace::Look, t0);
    r.engine.discover_if_due(t0);
    r.settle(t0);
    let before = r.board().figure(tab);
    assert!(before.drawn());
    r.engine.start_walk(&w);
    r.engine.put_walking();
    let during = r.board().figure(tab);
    assert_eq!(r.engine.records[&w].state, RootState::Walking);
    assert!(during.drawn(), "drawn while the rewalk runs");
    assert_eq!(during.bytes, before.bytes, "with its prior figure");
}

#[test]
fn case_and_firmlink_spellings_of_one_folder_count_once() {
    let outer = record(
        "/Users//a/SRC",
        RootState::Complete,
        Some(complete(500, Vec::new(), None)),
    );
    let inner = RootRecord {
        read_from: Some(PathBuf::from("/Users//a/SRC")),
        ..record(
            "/Users//a/src/aterm",
            RootState::Complete,
            Some(complete(300, Vec::new(), None)),
        )
    };
    let data = RootRecord {
        read_from: Some(PathBuf::from("/Users//a/SRC")),
        ..record(
            "/System/Volumes/Data/Users//a/src/x",
            RootState::Complete,
            Some(complete(100, Vec::new(), None)),
        )
    };
    let roots = [
        standing(Path::new("/Users//a/SRC"), Pool::Same, Some(&outer)),
        standing(Path::new("/Users//a/src/aterm"), Pool::Same, Some(&inner)),
        standing(
            Path::new("/System/Volumes/Data/Users//a/src/x"),
            Pool::Same,
            Some(&data),
        ),
    ];
    let folded = scope_figure(&roots, true);
    assert_eq!(folded.bytes, 500, "both spellings lie inside ~/SRC");
    // Negative control: case-sensitive nesting counts ~/SRC and ~/src/aterm twice.
    let exact = scope_figure(&roots, false);
    assert!(exact.bytes > folded.bytes, "{exact:?}");
}

#[test]
fn a_root_inside_a_refused_or_too_broad_root_still_counts() {
    let home = PathBuf::from("/Users//a");
    let w = home.join("w");
    let board = Published {
        tabs: vec![TabFacts {
            id: tab_id(1),
            label: "Tab 1".into(),
            number: Some(1),
            window: 1,
            panes: vec![1, 2],
        }],
        panes: BTreeMap::from([
            (
                1,
                PaneView {
                    members: vec![Claim::of(home.clone(), RootKind::Folder, false)],
                    ..PaneView::unseen(1)
                },
            ),
            (
                2,
                PaneView {
                    members: vec![Claim::of(w.clone(), RootKind::Project, false)],
                    ..PaneView::unseen(2)
                },
            ),
        ]),
        pools: BTreeMap::from([(home.clone(), Pool::Same), (w.clone(), Pool::Same)]),
        records: BTreeMap::from([
            (
                home.clone(),
                Arc::new(RootRecord {
                    state: RootState::OneLevel,
                    ..RootRecord::queued(home.clone())
                }),
            ),
            (
                w.clone(),
                Arc::new(RootRecord {
                    state: RootState::Complete,
                    complete: Some(complete(4_000, Vec::new(), None)),
                    ..RootRecord::queued(w.clone())
                }),
            ),
        ]),
        discovered: BTreeSet::from([DiskScope::Aterm]),
        fold_case: true,
        ..Published::default()
    };
    let report = build_report(DiskScope::Tab(tab_id(1)), &board, WalkerState::Running);
    let row = report
        .roots
        .iter()
        .find(|r| r.claim.path == w)
        .expect("w's row");
    assert_eq!(row.inside, None, "a too-broad root parents nothing");
    assert_eq!(report.total_bytes, 4_000);
    assert!(board.figure(DiskScope::Tab(tab_id(1))).drawn());
}

/// A small deterministic generator for the property tests.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// SHARED IS BY TAB (system-awareness §10.4 as F2a amends it): two panes of one tab at one
/// folder share nothing; a folder another tab claims inside it is shared, and is counted in the
/// least tab that holds it whole; each tab's exclusive bytes and aterm's shared bytes balance
/// against aterm's. Negative control: the same two panes in two tabs share the folder whole.
#[test]
fn shared_is_by_tab_and_each_tabs_exclusive_bytes_balance_against_atermss() {
    let p = |s: &str| PathBuf::from(s);
    let tab = |n: u64, panes: &[u64]| TabFacts {
        id: tab_id(n),
        label: format!("Tab {n}"),
        number: usize::try_from(n).ok(),
        window: 1,
        panes: panes.to_vec(),
    };
    let view = |key: u64, at: &[&str]| {
        (
            key,
            PaneView {
                members: at
                    .iter()
                    .map(|a| Claim::of(p(a), RootKind::Project, false))
                    .collect(),
                ..PaneView::unseen(key)
            },
        )
    };
    let records = BTreeMap::from([
        (
            p("/w"),
            Arc::new(record(
                "/w",
                RootState::Complete,
                Some(complete(1_000, Vec::new(), None)),
            )),
        ),
        (
            p("/w/sub"),
            Arc::new(RootRecord {
                read_from: Some(p("/w")),
                ..record(
                    "/w/sub",
                    RootState::Complete,
                    Some(complete(300, Vec::new(), None)),
                )
            }),
        ),
        (
            p("/x"),
            Arc::new(record(
                "/x",
                RootState::Complete,
                Some(complete(50, Vec::new(), None)),
            )),
        ),
    ]);
    let board = |tabs: Vec<TabFacts>, panes: Vec<(u64, PaneView)>| Published {
        tabs,
        panes: panes.into_iter().collect(),
        pools: ["/w", "/w/sub", "/x"]
            .iter()
            .map(|a| (p(a), Pool::Same))
            .collect(),
        records: records.clone(),
        discovered: BTreeSet::from([DiskScope::Aterm]),
        fold_case: true,
        ..Published::default()
    };
    let report = |b: &Published, scope| build_report(scope, b, WalkerState::Running);
    let (one, two) = (DiskScope::Tab(tab_id(1)), DiskScope::Tab(tab_id(2)));
    // Tab 1: panes 1 and 2 at /w. Tab 2: pane 3 at /w/sub (read from /w's walk) and /x.
    let b = board(
        vec![tab(1, &[1, 2]), tab(2, &[3])],
        vec![
            view(1, &["/w"]),
            view(2, &["/w"]),
            view(3, &["/w/sub", "/x"]),
        ],
    );
    let (t1, t2, all) = (
        report(&b, one),
        report(&b, two),
        report(&b, DiskScope::Aterm),
    );
    let row = |r: &DiskReport, at: &str| {
        r.roots
            .iter()
            .find(|row| row.claim.path == p(at))
            .cloned()
            .unwrap_or_else(|| panic!("no row for {at}: {r:#?}"))
    };
    let w = row(&t1, "/w");
    assert_eq!(
        (w.counted_in, w.shared_with.clone(), w.shared_alloc),
        (Some(1), vec![3], 300),
        "Tab 2's /w/sub lies inside it; Tab 1's own two panes are one claimer"
    );
    assert_eq!(
        (t1.total_bytes, t1.shared_bytes, t1.exclusive_bytes),
        (1_000, 300, 700)
    );
    let sub = row(&t2, "/w/sub");
    assert_eq!(
        (sub.counted_in, sub.shared_with.clone(), sub.shared_alloc),
        (Some(1), vec![1], 300),
        "Tab 1 holds it whole"
    );
    assert_eq!(row(&t2, "/x").shared_alloc, 0);
    assert_eq!(
        (t2.total_bytes, t2.shared_bytes, t2.exclusive_bytes),
        (350, 300, 50)
    );
    assert_eq!(row(&all, "/w").shared_with, vec![1, 3]);
    assert_eq!(
        (all.total_bytes, all.shared_bytes, all.exclusive_bytes),
        (1_050, 300, 750)
    );
    assert_eq!(
        t1.exclusive_bytes + t2.exclusive_bytes + all.shared_bytes,
        all.total_bytes,
        "every byte once"
    );
    // Tab 1 alone: its two panes at one folder share nothing.
    let alone = board(
        vec![tab(1, &[1, 2])],
        vec![view(1, &["/w"]), view(2, &["/w"])],
    );
    let t1 = report(&alone, one);
    assert_eq!((t1.shared_bytes, t1.exclusive_bytes), (0, 1_000));
    assert!(t1.roots[0].shared_with.is_empty(), "{t1:#?}");
    assert_eq!(report(&alone, DiskScope::Aterm).shared_bytes, 0);
    // Negative control: the same panes in two tabs share /w whole.
    let apart = board(
        vec![tab(1, &[1]), tab(3, &[2])],
        vec![view(1, &["/w"]), view(2, &["/w"])],
    );
    let t1 = report(&apart, one);
    assert_eq!((t1.shared_bytes, t1.exclusive_bytes), (1_000, 0));
    assert_eq!(t1.roots[0].shared_with, vec![2]);
    assert_eq!(report(&apart, DiskScope::Aterm).shared_bytes, 1_000);
}

fn walk_tree(path: &Path, policy: &Policy) -> (RootRecord, Arc<aterm_diskwalk::Tree>) {
    let mut walk = aterm_diskwalk::Walk::begin(&RootSpec::new(path), policy).expect("walk");
    while matches!(walk.diskwalk_slice(&mut Instant::now), Step::More { .. }) {}
    let id = (walk.root_dev(), walk.tree().root().ino);
    let tree = Arc::new(walk.into_tree());
    let at = Instant::now();
    let rec = RootRecord {
        state: RootState::Complete,
        id: Some(id),
        complete: Some(complete_of(&tree, DirId::ROOT, at, Duration::ZERO, None)),
        ..RootRecord::queued(path.to_path_buf())
    };
    (rec, tree)
}

fn board_of(
    tab: &[PathBuf],
    rest: &[PathBuf],
    records: &BTreeMap<PathBuf, Arc<RootRecord>>,
    fold: bool,
) -> Published {
    let claims = |ps: &[PathBuf]| -> Vec<Claim> {
        ps.iter()
            .map(|p| Claim::of(p.clone(), RootKind::Project, false))
            .collect()
    };
    Published {
        tabs: vec![
            TabFacts {
                id: tab_id(1),
                label: "Tab 1".into(),
                number: Some(1),
                window: 1,
                panes: vec![1],
            },
            TabFacts {
                id: tab_id(2),
                label: "Tab 2".into(),
                number: Some(2),
                window: 1,
                panes: vec![2],
            },
        ],
        panes: BTreeMap::from([
            (
                1,
                PaneView {
                    members: claims(tab),
                    ..PaneView::unseen(1)
                },
            ),
            (
                2,
                PaneView {
                    members: claims(rest),
                    ..PaneView::unseen(2)
                },
            ),
        ]),
        pools: tab
            .iter()
            .chain(rest)
            .map(|p| (p.clone(), Pool::Same))
            .collect(),
        records: records.clone(),
        discovered: BTreeSet::from([DiskScope::Aterm]),
        fold_case: fold,
        ..Published::default()
    }
}

/// THE DISK BAR NESTS THE TAB'S BYTES IN ATERM'S IN THE MAC'S USED: over random member sets of
/// a real scratch tree, random walk orders with growth and new folders between walks, random
/// walks left running, each scope read from one board, B(tab) ≤ B(aterm), and the clamp draws
/// T ≤ A ≤ used.
#[test]
fn the_disk_bar_nests_the_tabs_bytes_in_atermss_in_the_macs_used() {
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let b = base(&tmp);
    let home = b.join("home");
    let fold = cfg!(target_vendor = "apple");
    let policy = policy_for(&home);
    let mut rng = Lcg(0x5eed_f2a0);
    for trial in 0..40 {
        let r = home.join(format!("t{trial}"));
        let rel = ["a", "a/b", "a/b/c", "a/d", "e", "e/f"];
        for d in &rel[..5] {
            mkdir(&r.join(d));
            write(&r.join(d).join("x"), 4_096 * (1 + rng.below(4) as usize));
        }
        let universe: Vec<PathBuf> = rel.iter().map(|d| r.join(d)).collect();
        let aterm: Vec<PathBuf> = universe
            .iter()
            .filter(|_| rng.below(3) > 0)
            .cloned()
            .collect();
        let tab: Vec<PathBuf> = aterm
            .iter()
            .filter(|_| rng.below(2) == 0)
            .cloned()
            .collect();
        let rest: Vec<PathBuf> = aterm.iter().filter(|p| !tab.contains(p)).cloned().collect();
        let mut plan = aterm.clone();
        plan.sort_by_key(|p| p.components().count());
        let mut kept = Kept::default();
        let mut records: BTreeMap<PathBuf, Arc<RootRecord>> = BTreeMap::new();
        for _ in 0..6 {
            match rng.below(4) {
                0 => {
                    // A folder created after the walks so far, and growth.
                    mkdir(&r.join("e/f"));
                    let grow = &universe[usize::try_from(rng.below(6)).expect("small")];
                    if grow.is_dir() {
                        write(&grow.join(format!("g{}", rng.next())), 8_192);
                    }
                }
                _ => {
                    let p = &plan[usize::try_from(rng.below(plan.len().max(1) as u64))
                        .expect("small")
                        .min(plan.len().saturating_sub(1))];
                    if plan.is_empty() || !p.is_dir() {
                        continue;
                    }
                    let (rec, tree) = walk_tree(p, &policy);
                    if rng.below(5) == 0 {
                        // A rewalk left running: it keeps the prior figure.
                        let prior = records.get(p).and_then(|r| r.complete.clone());
                        records.insert(
                            p.clone(),
                            Arc::new(RootRecord {
                                state: RootState::Walking,
                                partial: Some(sums(1 << 40)),
                                complete: prior,
                                ..RootRecord::queued(p.clone())
                            }),
                        );
                    } else {
                        records.insert(p.clone(), Arc::new(rec));
                        kept.trees.insert(p.clone(), tree);
                    }
                    rederive(&plan, &mut kept, &mut records, None, fold);
                }
            }
        }
        let board = board_of(&tab, &rest, &records, fold);
        let t = board.figure(DiskScope::Tab(tab_id(1)));
        let a = board.figure(DiskScope::Aterm);
        assert!(
            t.bytes <= a.bytes,
            "trial {trial}: tab {t:?} > aterm {a:?}\ntab {tab:?}\naterm {aterm:?}\n{records:#?}"
        );
        let used = a.bytes / 2 + rng.below(a.bytes.max(1));
        let c = clamp(Some(used), a.bytes, t.bytes);
        assert!(c.tab <= c.aterm && c.aterm <= used, "trial {trial}: {c:?}");
    }
    // Negative control: the two scopes read from two boards around a publish (a rewalk that
    // grew the tab's root) break the law.
    let r = home.join("two-boards");
    mkdir(&r);
    write(&r.join("x"), 4_096);
    let (first, _) = walk_tree(&r, &policy);
    let one = BTreeMap::from([(r.clone(), Arc::new(first))]);
    write(&r.join("y"), 65_536);
    let (second, _) = walk_tree(&r, &policy);
    let two = BTreeMap::from([(r.clone(), Arc::new(second))]);
    let roots = std::slice::from_ref(&r);
    let aterm_then = board_of(roots, &[], &one, fold).figure(DiskScope::Aterm);
    let tab_now = board_of(roots, &[], &two, fold).figure(DiskScope::Tab(tab_id(1)));
    assert!(tab_now.bytes > aterm_then.bytes);
}

#[test]
fn clones_past_used_clamp_atermss_fill_and_are_said() {
    let c = clamp(Some(1_000), 1_400, 1_200);
    assert_eq!(
        c,
        Clamped {
            aterm: 1_000,
            tab: 1_000,
            aterm_clamped: true,
            tab_clamped: true,
        }
    );
    let fits = clamp(Some(5_000), 1_400, 300);
    assert_eq!((fits.aterm, fits.tab), (1_400, 300));
    assert!(!fits.aterm_clamped && !fits.tab_clamped);
    assert_eq!(
        clamp(None, 1_400, 300).aterm,
        1_400,
        "no used read: no clamp"
    );
}

// ---------------------------------------------------------------------------------------------
// Paces and the glance budget.

#[test]
fn a_glance_rewalks_a_root_no_sooner_than_ten_minutes_or_fifty_times_its_walk() {
    let mut c = complete(1, Vec::new(), None);
    c.took = Duration::from_millis(200);
    assert_eq!(c.rewalk_at(Pace::Look), c.at + REWALK_AFTER);
    assert_eq!(c.rewalk_at(Pace::Glance), c.at + GLANCE_REWALK_AFTER);
    c.took = Duration::from_secs(4);
    assert_eq!(c.rewalk_at(Pace::Look), c.at + Duration::from_secs(80));
    c.took = Duration::from_secs(20);
    assert_eq!(c.rewalk_at(Pace::Glance), c.at + Duration::from_secs(1000));
    let now = c.at + Duration::from_secs(599);
    let plan = vec![PathBuf::from("/w")];
    let records = BTreeMap::from([(
        PathBuf::from("/w"),
        Arc::new(RootRecord {
            state: RootState::Complete,
            complete: Some(Complete {
                took: Duration::from_millis(200),
                ..c.clone()
            }),
            ..RootRecord::queued(PathBuf::from("/w"))
        }),
    )]);
    let none = BTreeSet::new();
    let pick = |pace: Pace, at: Instant| {
        let paces = BTreeMap::from([(PathBuf::from("/w"), pace)]);
        pick_root(
            &Pick {
                plan: &plan,
                paces: &paces,
                tab_held: &none,
                frees: &none,
                opens: at,
                fold: true,
            },
            &records,
            at,
        )
    };
    assert_eq!(pick(Pace::Glance, now), None, "599 s at glance pace");
    assert_eq!(pick(Pace::Look, now), Some(PathBuf::from("/w")));
    assert_eq!(
        pick(Pace::Glance, c.at + GLANCE_REWALK_AFTER),
        Some(PathBuf::from("/w"))
    );
}

#[test]
fn a_glance_walk_starts_only_while_glance_walks_took_under_twelve_seconds_of_the_last_ten_minutes()
{
    assert_eq!(
        (GLANCE_BUDGET, GLANCE_WINDOW),
        (Duration::from_secs(12), Duration::from_secs(600))
    );
    let t0 = Instant::now();
    let at = |s: u64| t0 + Duration::from_secs(s);
    let mut budget = GlanceBudget::default();
    assert!(budget.allows(at(0)));
    budget.charge(at(0), Duration::from_secs(7));
    assert!(budget.allows(at(1)));
    budget.charge(at(1), Duration::from_secs(6));
    assert!(!budget.allows(at(2)), "13 s of the window");
    assert!(budget.allows(at(600)), "the first charge aged out");
    // The schedule: a glance root waits while the budget is spent, a look root and the front
    // tab's first walk of a root that just joined do not.
    let plan: Vec<PathBuf> = ["/g", "/l", "/t"].iter().map(PathBuf::from).collect();
    let paces = BTreeMap::from([
        (PathBuf::from("/g"), Pace::Glance),
        (PathBuf::from("/l"), Pace::Look),
        (PathBuf::from("/t"), Pace::Glance),
    ]);
    let tab_held = BTreeSet::from([PathBuf::from("/t")]);
    let none = BTreeSet::new();
    let mut records: BTreeMap<PathBuf, Arc<RootRecord>> = BTreeMap::new();
    let pick = |records: &BTreeMap<PathBuf, Arc<RootRecord>>, budget: bool| {
        pick_root(
            &Pick {
                plan: &plan,
                paces: &paces,
                tab_held: &tab_held,
                frees: &none,
                opens: if budget { t0 } else { t0 + GLANCE_WINDOW },
                fold: true,
            },
            records,
            t0,
        )
    };
    assert_eq!(
        pick(&records, false),
        Some(PathBuf::from("/t")),
        "exempt, and first"
    );
    records.insert(
        PathBuf::from("/t"),
        Arc::new(record(
            "/t",
            RootState::Complete,
            Some(complete(1, Vec::new(), None)),
        )),
    );
    assert_eq!(pick(&records, false), Some(PathBuf::from("/l")));
    records.insert(
        PathBuf::from("/l"),
        Arc::new(record(
            "/l",
            RootState::Complete,
            Some(complete(1, Vec::new(), None)),
        )),
    );
    assert_eq!(pick(&records, false), None, "/g waits for the budget");
    // Negative control: an unmetered schedule walks it.
    assert_eq!(pick(&records, true), Some(PathBuf::from("/g")));
}

/// The budget opens when enough of the window's charges age out for the rest to take under
/// [`GLANCE_BUDGET`]: [`GlanceBudget::opens_at`] is the first instant [`GlanceBudget::allows`].
#[test]
fn the_glance_budget_opens_when_enough_charges_age_out_of_its_window() {
    let t0 = Instant::now();
    let at = |s: u64| t0 + Duration::from_secs(s);
    assert_eq!(GlanceBudget::default().opens_at(at(0)), at(0), "empty");
    for (charges, opens) in [
        (&[(0, 7), (1, 6), (2, 4)][..], at(600)),
        (&[(0, 7), (1, 6), (2, 6)][..], at(601)),
        (&[(0, 5), (1, 6)][..], at(3)),
    ] {
        let mut budget = GlanceBudget::default();
        for &(t, c) in charges {
            budget.charge(at(t), Duration::from_secs(c));
        }
        assert_eq!(budget.opens_at(at(3)), opens, "{charges:?}");
        for s in [3, 300, 599, 600, 601, 1200] {
            let open = budget.opens_at(at(s));
            assert_eq!(
                budget.clone().allows(at(s)),
                open <= at(s),
                "{charges:?} at {s}"
            );
            assert!(budget.clone().allows(open), "{charges:?} opens at {s}");
            if open > at(s) {
                let before = open.checked_sub(Duration::from_millis(1)).expect("past");
                assert!(!budget.clone().allows(before), "{charges:?} no sooner");
            }
        }
    }
}

/// THE WALKER SLEEPS UNTIL A WALK IT MAY START IS OWED ([`wake_at`]): a glance root owed again
/// while the budget holds it back wakes the thread when the budget opens, never sooner; a look
/// root owed again and a demanded tab's first walk wake it now. The law: [`pick_root`] owes a
/// walk exactly when the wake is now or past, budget open or spent. Negative control: the wake
/// read with the budget left out (what `next_due` read) is past while [`pick_root`] starts
/// nothing, so the thread woke every millisecond until the budget opened.
#[test]
fn a_glance_root_the_budget_holds_back_wakes_the_walker_when_the_budget_opens() {
    let now = Instant::now() + Duration::from_secs(3600);
    let ago = |s: u64| now.checked_sub(Duration::from_secs(s)).expect("past");
    let mut budget = GlanceBudget::default();
    budget.charge(ago(2), Duration::from_secs(13));
    let opens = budget.opens_at(now);
    assert_eq!(opens, ago(2) + GLANCE_WINDOW, "13 s of the window");
    let open = ago(3600);
    let walked = |path: &str, s: u64| {
        let mut c = complete(1, Vec::new(), None);
        c.at = ago(s);
        c.took = Duration::from_millis(5);
        Arc::new(record(path, RootState::Complete, Some(c)))
    };
    let p = PathBuf::from;
    let glance = (p("/g"), walked("/g", 700));
    let look = (p("/l"), walked("/l", 40));
    let derived = (
        p("/g/in"),
        Arc::new(RootRecord {
            read_from: Some(p("/g")),
            ..(*walked("/g/in", 700)).clone()
        }),
    );
    let refused = (
        p("/r"),
        Arc::new(RootRecord {
            asked: Some(ago(700)),
            ..record("/r", RootState::NotMeasured(Refusal::Protected), None)
        }),
    );
    let paces = BTreeMap::from([
        (p("/g"), Pace::Glance),
        (p("/g/in"), Pace::Glance),
        (p("/l"), Pace::Look),
        (p("/r"), Pace::Glance),
        (p("/q"), Pace::Glance),
        (p("/t"), Pace::Glance),
        (p("/g/n"), Pace::Look),
    ]);
    let tab_held = BTreeSet::from([p("/t")]);
    let none = BTreeSet::new();
    let case = |plan: &[&str], records: &[&(PathBuf, Arc<RootRecord>)]| {
        let plan: Vec<PathBuf> = plan.iter().map(PathBuf::from).collect();
        let records: BTreeMap<PathBuf, Arc<RootRecord>> = records
            .iter()
            .map(|(k, v)| (k.clone(), Arc::clone(v)))
            .collect();
        (plan, records)
    };
    let run = |plan: &[PathBuf], records: &BTreeMap<PathBuf, Arc<RootRecord>>, opens: Instant| {
        let pick = Pick {
            plan,
            paces: &paces,
            tab_held: &tab_held,
            frees: &none,
            opens,
            fold: true,
        };
        (pick_root(&pick, records, now), wake_at(&pick, records, now))
    };
    let cases = [
        (
            case(&["/g"], &[&glance]),
            Some(opens),
            "an overdue glance root",
        ),
        (
            case(&["/g", "/g/in"], &[&glance, &derived]),
            Some(opens),
            "a root read from it adds no wake",
        ),
        (
            case(&["/r"], &[&refused]),
            Some(opens),
            "a refusal asked again",
        ),
        (
            case(&["/q"], &[]),
            Some(opens),
            "a glance root never walked",
        ),
        (
            case(&["/g", "/l"], &[&glance, &look]),
            Some(ago(10)),
            "a look root",
        ),
        (
            case(&["/g", "/t"], &[&glance]),
            Some(now),
            "a tab's first walk",
        ),
        (
            case(&["/g", "/g/n"], &[]),
            Some(opens),
            "a look root inside a glance root never walked waits on its walk",
        ),
    ];
    for ((plan, records), wake, why) in &cases {
        let (picked, woke) = run(plan, records, opens);
        assert_eq!(woke, *wake, "{why}");
        assert_eq!(
            picked.is_some(),
            woke.is_some_and(|t| t <= now),
            "{why}: owed now exactly when the wake is now"
        );
        let (picked, woke) = run(plan, records, open);
        assert_eq!(
            picked.is_some(),
            woke.is_some_and(|t| t <= now),
            "{why}, budget open"
        );
    }
    // Negative control: the budget left out of the wake, as `next_due` read it.
    let ((plan, records), _, _) = &cases[0];
    let (picked, metered) = run(plan, records, opens);
    let (_, unmetered) = run(plan, records, open);
    assert_eq!(picked, None, "the budget holds /g back");
    assert!(
        unmetered.is_some_and(|t| t < now) && metered.is_some_and(|t| t > now),
        "unmetered {unmetered:?}, metered {metered:?}"
    );
}

/// The engine composes [`wake_at`]: a tab's root at glance pace, walked, then owed again while
/// the budget is spent, wakes [`Engine::next_due`] when the budget opens and starts no walk
/// before. Negative control: with the budget open the same root starts now.
#[test]
fn the_engine_sleeps_past_a_spent_budget_until_it_opens() {
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let b = base(&tmp);
    let home = b.join("home");
    let w = home.join("src/w");
    mkdir(&w.join(".git"));
    write(&w.join("f"), 4_000);
    let mut r = Rig::new(&home);
    r.facts(&[(1, 1, &w)], &[], &[]);
    let tab = DiskScope::Tab(tab_id(1));
    let t0 = Instant::now();
    r.demand(tab, Pace::Glance, t0);
    r.engine.discover_if_due(t0);
    assert_eq!(
        r.settle(t0),
        std::slice::from_ref(&w),
        "the tab's first walk"
    );
    let later = t0 + GLANCE_REWALK_AFTER + Duration::from_secs(100);
    r.demand(tab, Pace::Glance, later);
    r.engine.budget.charge(later, Duration::from_secs(13));
    let opens = later + GLANCE_WINDOW;
    assert_eq!(r.engine.next_root(later), None, "the budget holds it back");
    assert_eq!(r.engine.next_due(later), Some(opens));
    r.engine.budget = GlanceBudget::default();
    assert_eq!(r.engine.next_root(later), Some(w));
    assert!(r.engine.next_due(later).is_some_and(|t| t <= later));
}

#[test]
fn the_front_tabs_new_root_is_walked_first() {
    let now = Instant::now();
    let plan: Vec<PathBuf> = ["/a", "/z", "/q/deep/new"]
        .iter()
        .map(PathBuf::from)
        .collect();
    let paces: BTreeMap<PathBuf, Pace> = plan.iter().map(|p| (p.clone(), Pace::Look)).collect();
    let tab_held = BTreeSet::from([PathBuf::from("/q/deep/new")]);
    let none = BTreeSet::new();
    let pick = |tab_held: &BTreeSet<PathBuf>| {
        pick_root(
            &Pick {
                plan: &plan,
                paces: &paces,
                tab_held,
                frees: &none,
                opens: now,
                fold: true,
            },
            &BTreeMap::new(),
            now,
        )
    };
    assert_eq!(pick(&tab_held), Some(PathBuf::from("/q/deep/new")));
    assert_eq!(
        pick(&none),
        Some(PathBuf::from("/a")),
        "else outermost first"
    );
}

#[test]
fn a_nested_root_waits_for_its_enclosing_walk_and_frees_rewalk_now() {
    let now = Instant::now();
    let plan: Vec<PathBuf> = ["/z", "/z/t"].iter().map(PathBuf::from).collect();
    let paces: BTreeMap<PathBuf, Pace> = plan.iter().map(|p| (p.clone(), Pace::Look)).collect();
    let none = BTreeSet::new();
    let pick = |records: &BTreeMap<PathBuf, Arc<RootRecord>>, frees: &BTreeSet<PathBuf>| {
        pick_root(
            &Pick {
                plan: &plan,
                paces: &paces,
                tab_held: &none,
                frees,
                opens: now,
                fold: true,
            },
            records,
            now,
        )
    };
    let walking = BTreeMap::from([(
        PathBuf::from("/z"),
        Arc::new(record("/z", RootState::Walking, None)),
    )]);
    assert_eq!(pick(&walking, &none), None, "/z/t waits for /z's walk");
    let done = BTreeMap::from([(
        PathBuf::from("/z"),
        Arc::new(record(
            "/z",
            RootState::Complete,
            Some(complete(1, Vec::new(), None)),
        )),
    )]);
    assert_eq!(pick(&done, &none), Some(PathBuf::from("/z/t")));
    let derived = BTreeMap::from([
        (
            PathBuf::from("/z"),
            Arc::new(record(
                "/z",
                RootState::Complete,
                Some(complete(1, Vec::new(), None)),
            )),
        ),
        (
            PathBuf::from("/z/t"),
            Arc::new(RootRecord {
                read_from: Some(PathBuf::from("/z")),
                ..record(
                    "/z/t",
                    RootState::Complete,
                    Some(complete(1, Vec::new(), None)),
                )
            }),
        ),
    ]);
    assert_eq!(
        pick(&derived, &none),
        None,
        "a derived root is never walked alone"
    );
    let frees = BTreeSet::from([PathBuf::from("/z")]);
    assert_eq!(
        pick(&derived, &frees),
        Some(PathBuf::from("/z")),
        "a sizes-only walk is owed now when its frees are asked"
    );
}

#[test]
fn a_hidden_or_occluded_deck_demands_no_disk_scope() {
    let front = Some(tab_id(3));
    let w = |drawn, occluded, disk_lens| DeckWindow {
        drawn,
        occluded,
        front,
        disk_lens,
    };
    assert_eq!(
        deck_asks(true, &[w(true, false, false)]),
        vec![
            (DiskScope::Tab(tab_id(3)), Pace::Glance),
            (DiskScope::Aterm, Pace::Glance)
        ]
    );
    assert_eq!(
        deck_asks(true, &[w(true, false, true)]),
        vec![
            (DiskScope::Tab(tab_id(3)), Pace::Look),
            (DiskScope::Aterm, Pace::Look)
        ],
        "an open Disk lens looks"
    );
    assert!(
        deck_asks(true, &[w(true, true, false)]).is_empty(),
        "occluded"
    );
    assert!(
        deck_asks(true, &[w(false, false, false)]).is_empty(),
        "hidden"
    );
    assert!(deck_asks(false, &[w(true, false, false)]).is_empty(), "off");
    assert_eq!(DECK_ASK_EVERY * 2, DEMAND_TTL);
}

// ---------------------------------------------------------------------------------------------
// The walker thread.

/// The walker thread sizes a demanded tab's project from its OSC 7 report, reads the pinned
/// target from that walk, claims the build mirror, and names a protected pane without opening it.
#[test]
fn the_walker_thread_sizes_a_demanded_tab_and_reads_nested_roots_from_one_walk() {
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let b = base(&tmp);
    let home = b.join("home");
    let w = home.join("w");
    for d in [
        "w/.git",
        "w/crates/x",
        "w/target/debug",
        "Documents/proj/.git",
    ] {
        mkdir(&home.join(d));
    }
    write(&w.join("crates/x/lib.rs"), 3_000);
    write(&w.join("target/debug/bin"), 50_000);
    let mirror = build_mirror(&atpkg::build_cache::cache_root(&home), &w);
    mkdir(&mirror.join(".cargo-build"));
    write(&mirror.join(".cargo-build/obj"), 20_000);
    let shared: &'static Shared = Box::leak(Box::new(Shared::new()));
    let (one, two) = (tab_id(1), tab_id(2));
    shared.demand(
        DiskScope::Tab(one),
        Detail::Frees,
        Pace::Look,
        Instant::now(),
    );
    let from_walker = spawn_walker(shared, None, TEST_WALKER_CLASS).expect("the walker starts");
    assert_eq!(shared.walker_state(), WalkerState::Running);
    let osc7 = |p: &Path| report(&p.display().to_string(), true);
    shared.send(ToWalker::Facts(Facts {
        panes: vec![
            pane(1, u32::MAX - 7, u32::MAX - 7, osc7(&w)),
            pane(
                2,
                u32::MAX - 8,
                u32::MAX - 8,
                osc7(&home.join("Documents/proj")),
            ),
        ],
        tabs: vec![
            TabFacts {
                id: one,
                label: "Tab 1".into(),
                number: Some(1),
                window: 1,
                panes: vec![1],
            },
            TabFacts {
                id: two,
                label: "Tab 2".into(),
                number: Some(2),
                window: 1,
                panes: vec![2],
            },
        ],
        home: Some(home.clone()),
        ..Facts::default()
    }));
    shared.demand(
        DiskScope::Tab(two),
        Detail::Sizes,
        Pace::Look,
        Instant::now(),
    );
    assert!(shared.demanded(Instant::now()));
    let deadline = Instant::now() + Duration::from_secs(60);
    let (board, a, b2) = loop {
        shared.absorb(&from_walker);
        let board = shared.board();
        let a = build_report(DiskScope::Tab(one), &board, WalkerState::Running);
        let b2 = build_report(DiskScope::Tab(two), &board, WalkerState::Running);
        let settled = !a.walking
            && !a.roots.is_empty()
            && board.panes.get(&2).is_some_and(|p| p.why.is_some());
        if settled {
            break (board, a, b2);
        }
        assert!(
            Instant::now() < deadline,
            "the walker never settled: {a:#?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    shared.send(ToWalker::Stop);
    let row = |p: &Path| {
        a.roots
            .iter()
            .find(|row| row.claim.path == p)
            .cloned()
            .unwrap_or_else(|| panic!("no row for {}: {a:#?}", p.display()))
    };
    let repo = row(&w);
    assert_eq!(repo.claim.kinds, BTreeSet::from([RootKind::Project]));
    assert_eq!(board.panes[&1].from, Some(LiveFrom::Osc7));
    let c = repo.record.complete.as_ref().expect("complete");
    assert!(c.sums.alloc_fair >= 53_000, "{c:?}");
    assert_eq!(
        c.detail,
        Detail::Frees,
        "the ask made before the thread ran was heard"
    );
    assert!(c.links.is_some(), "a Frees walk keeps its link totals");
    let build = row(&mirror);
    assert_eq!(build.claim.kinds, BTreeSet::from([RootKind::Build]));
    assert_eq!(build.inside, None);
    assert_eq!(
        a.total_bytes,
        c.sums.alloc_fair + build.alloc(),
        "the pinned target inside the project is counted there, once"
    );
    assert_eq!(a.total_bytes, board.figure(DiskScope::Tab(one)).bytes);
    assert_eq!(
        board.panes[&2].why,
        Some(PaneWhy::Refused(Refusal::Protected))
    );
    assert!(b2.roots.is_empty(), "named, not a root: {b2:#?}");
}

/// THE WALKER THREAD RUNS UNDER THE DISK POLICY ITS CLASS NAMES: spawned
/// with [`aterm_diskwalk::DiskIo::Throttled`] (the product's, as
/// [`WALKER_CLASS`] names it) its own disk policy, read on that thread,
/// is `IOPOL_THROTTLE`; spawned with `Inherited` it is not. Each runs at
/// the test class's CPU role, so the answer waits on no background class,
/// and the wait for it is a one-minute hang detector. A `walker_main` that
/// dropped its `DiskIo` for either constant fails one of the two.
#[test]
fn the_walker_thread_runs_under_the_disk_policy_its_class_names() {
    use aterm_diskwalk::DiskIo;
    assert_eq!(WALKER_CLASS.disk, DiskIo::Throttled, "the product's reads");
    for (disk, throttled) in [(DiskIo::Throttled, true), (DiskIo::Inherited, false)] {
        let shared: &'static Shared = Box::leak(Box::new(Shared::new()));
        let class = WalkerClass {
            role: TEST_WALKER_CLASS.role,
            disk,
        };
        let _from_walker = spawn_walker(shared, None, class).expect("the walker starts");
        let (reply, answer) = std::sync::mpsc::channel();
        shared.send(ToWalker::ReadDiskPolicy(reply));
        let read = answer
            .recv_timeout(Duration::from_secs(60))
            .expect("the walker thread answers");
        shared.send(ToWalker::Stop);
        let expected = cfg!(target_vendor = "apple").then_some(throttled);
        assert_eq!(read, expected, "{disk:?}");
    }
}

/// THE GLANCE BUDGET, MEASURED: the walker thread itself ([`spawn_walker`] in the product's
/// [`WALKER_CLASS`]) while a drawn deck asks this checkout's tab and aterm at glance pace every
/// [`DECK_ASK_EVERY`], aterm's own folders named as the product names them (its state folder,
/// the build cache, atpkg's prefix): the first walks, then [`GLANCE_REWALK_AFTER`] and two
/// minutes more once they stand, so the first glance rewalks and any start the budget holds back
/// fall inside. The thread's CPU, its sleeps and the budget are read on it ([`ToWalker::Probe`])
/// every ten seconds, each answer inside the one-minute hang detector, as is the first walks'
/// progress. Run on demand through `targo --unverified test` (`--ignored --nocapture`): it takes
/// over twelve minutes and walks the whole build cache, and its figures are the step's
/// measurement.
#[test]
#[ignore = "a measurement of minutes, run on demand for the step's record"]
fn glance_pace_on_a_real_tree_stays_inside_the_budget() {
    let tree = canonical(
        &aterm_tempfile::manifest_dir()
            .join("../..")
            .canonicalize()
            .expect("the checkout"),
    )
    .expect("absolute");
    let home = aterm_types::dirs::home_dir().expect("a home");
    // The product's state folder: a test binary's `dirs::state_dir` is its scratch root's.
    let state = aterm_types::dirs::resolve_state_dir(
        None,
        aterm_types::dirs::StatePlatform {
            home: Some(home.clone()),
            xdg_state_home: std::env::var_os("XDG_STATE_HOME").map(PathBuf::from),
            local_app_data: std::env::var_os("LOCALAPPDATA").map(PathBuf::from),
        },
    );
    let own = own_paths(Some(&home), state, cfg!(target_os = "macos"));
    let shared: &'static Shared = Box::leak(Box::new(Shared::new()));
    let from_walker = spawn_walker(shared, None, WALKER_CLASS).expect("the walker starts");
    let tab = DiskScope::Tab(tab_id(1));
    shared.send(ToWalker::Facts(Facts {
        panes: vec![pane(
            1,
            u32::MAX - 7,
            u32::MAX - 7,
            report(&tree.display().to_string(), true),
        )],
        tabs: vec![TabFacts {
            id: tab_id(1),
            label: "Tab 1".into(),
            number: Some(1),
            window: 1,
            panes: vec![1],
        }],
        home: Some(home.clone()),
        own: own.clone(),
        ..Facts::default()
    }));
    let probe = || -> Probe {
        let (tx, rx) = std::sync::mpsc::channel();
        shared.send(ToWalker::Probe(tx));
        rx.recv_timeout(Duration::from_secs(60))
            .expect("the walker answers inside the hang detector")
    };
    let ask = |at: Instant| {
        for scope in [tab, DiskScope::Aterm] {
            shared.demand(scope, Detail::Sizes, Pace::Glance, at);
        }
    };
    let mut asked = Instant::now();
    ask(asked);
    let (p0, wall0) = (probe(), Instant::now());
    let cpu = |p: &Probe| p.cpu.expect("the walker thread's CPU");
    let mut samples: Vec<(Duration, Probe)> = Vec::new();
    let mut sampled = wall0;
    let mut moved = (Instant::now(), (0u64, 0usize, 0u64));
    let mut tick = |asked: &mut Instant, samples: &mut Vec<(Duration, Probe)>| {
        std::thread::sleep(Duration::from_secs(1));
        let now = Instant::now();
        if now.duration_since(*asked) >= DECK_ASK_EVERY {
            *asked = now;
            ask(now);
        }
        if now.duration_since(sampled) >= Duration::from_secs(10) {
            sampled = now;
            samples.push((now.duration_since(wall0), probe()));
        }
        shared.absorb(&from_walker);
        shared.report(DiskScope::Aterm)
    };
    // The first walks: until aterm's scope stands, discovered and walking nothing.
    let first = loop {
        let all = tick(&mut asked, &mut samples);
        if !all.walking && !all.roots.is_empty() {
            break (probe(), wall0.elapsed(), all);
        }
        let held = samples.last().is_some_and(|(_, p)| p.held_back);
        let progress = (
            all.roots
                .iter()
                .map(|r| r.record.walked_entries)
                .sum::<u64>(),
            all.roots
                .iter()
                .filter(|r| r.record.complete.is_some())
                .count(),
            samples.last().map_or(0, |(_, p)| p.starts),
        );
        if progress != moved.1 || held {
            moved = (Instant::now(), progress);
        }
        assert!(
            moved.0.elapsed() < Duration::from_secs(60),
            "the first walks made no progress for a minute: {all:#?}"
        );
    };
    let (p1, first_wall, stood) = first;
    let wall1 = Instant::now();
    let mut steady_samples: Vec<(Duration, Probe)> = Vec::new();
    while wall1.elapsed() < GLANCE_REWALK_AFTER + Duration::from_secs(120) {
        tick(&mut asked, &mut steady_samples);
    }
    let p2 = probe();
    let wall = wall1.elapsed();
    shared.send(ToWalker::Stop);
    let board = shared.board();
    let roots: Vec<String> = stood
        .roots
        .iter()
        .map(|r| {
            format!(
                "{} {:?} {:?} {} B",
                r.claim.path.display(),
                r.claim.kinds,
                r.record.state,
                r.alloc()
            )
        })
        .collect();
    let steady_cpu = cpu(&p2).saturating_sub(cpu(&p1));
    let held: Vec<&(Duration, Probe)> = steady_samples
        .iter()
        .chain(&samples)
        .filter(|(_, p)| p.held_back)
        .collect();
    for (t, p) in samples.iter().chain(&steady_samples) {
        eprintln!(
            "glance sample: t_s={} cpu_ms={} taken_ms={} held_back={} sleeps={} starts={}",
            t.as_secs(),
            cpu(p).saturating_sub(cpu(&p0)).as_millis(),
            p.glance_taken.as_millis(),
            p.held_back,
            p.sleeps,
            p.starts
        );
    }
    let fig = board.figure(DiskScope::Aterm);
    let tab_fig = board.figure(tab);
    eprintln!(
        "glance measurement: own={own:?} roots={roots:#?} first_walks_cpu_ms={} \
         first_walks_wall_ms={} first_starts={} steady_cpu_ms={} steady_wall_ms={} \
         steady_share={:.5} steady_sleeps={} steady_starts={} held_back_samples={} \
         aterm_bytes={} aterm_counted={} tab_bytes={} tab_counted={}",
        cpu(&p1).saturating_sub(cpu(&p0)).as_millis(),
        first_wall.as_millis(),
        p1.starts - p0.starts,
        steady_cpu.as_millis(),
        wall.as_millis(),
        steady_cpu.as_secs_f64() / wall.as_secs_f64(),
        p2.sleeps - p1.sleeps,
        p2.starts - p1.starts,
        held.len(),
        fig.bytes,
        fig.counted,
        tab_fig.bytes,
        tab_fig.counted
    );
    assert!(tab_fig.drawn() && fig.drawn(), "{tab_fig:?} {fig:?}");
    assert!(
        steady_cpu.as_secs_f64() / wall.as_secs_f64() < 0.02,
        "glance pace stays inside 2 % of a core once the first walks stand"
    );
    let rate =
        f64::from(u32::try_from(p2.sleeps - p1.sleeps).unwrap_or(u32::MAX)) / wall.as_secs_f64();
    assert!(
        rate < 2.0,
        "the thread sleeps until something is owed: {rate:.1} sleeps/s"
    );
}

// ---------------------------------------------------------------------------------------------
// The build folder hazards (§10.8) and growth.

fn huge(path: &str, entries: u32) -> (PathBuf, HugeDir) {
    (
        PathBuf::from(path),
        HugeDir {
            path: PathBuf::from(path),
            entries,
            executables: true,
            warning: entries >= aterm_diskwalk::HUGE_WARN,
        },
    )
}

/// Huge folders are raised at 100,000 entries, kept down to 75,000, and scoped to their walked root.
#[test]
fn the_huge_folder_set_has_hysteresis() {
    let root = Path::new("/w");
    let none = BTreeMap::new();
    let found = BTreeMap::from([huge("/w/a", 104_000), huge("/w/b", 80_000)]);
    let raised = fold_raised(&none, root, &found);
    assert_eq!(raised.keys().collect::<Vec<_>>(), [Path::new("/w/a")]);
    let raised = fold_raised(&raised, root, &BTreeMap::from([huge("/w/a", 76_000)]));
    assert!(raised.contains_key(Path::new("/w/a")), "kept at 76,000");
    let mut with_other = raised.clone();
    with_other.extend([huge("/x/c", 120_000)]);
    let lowered = fold_raised(&with_other, root, &BTreeMap::new());
    assert_eq!(
        lowered.keys().collect::<Vec<_>>(),
        [Path::new("/x/c")],
        "a walk of /w lowers /w/a and leaves /x alone"
    );
}

/// The slow tier folds outside profiles with the same hysteresis without disturbing walked roots.
#[test]
fn the_slow_tier_folds_outside_profiles_with_the_same_hysteresis() {
    let counted = |rows: &[(&str, u32)]| -> BTreeMap<PathBuf, Option<HugeDir>> {
        rows.iter()
            .map(|(p, n)| {
                let (path, h) = huge(p, *n);
                (path, (*n >= aterm_diskwalk::HUGE_NOTE).then_some(h))
            })
            .collect()
    };
    let walked = BTreeMap::from([huge("/w/target/debug/deps", 110_000)]);
    let (raised, mine) = fold_outside(
        &walked,
        &BTreeSet::new(),
        &counted(&[
            ("/a/target/debug/deps", 104_000),
            ("/b/target/release/deps", 80_000),
        ]),
        &[PathBuf::from("/w")],
    );
    assert_eq!(
        raised.keys().map(PathBuf::as_path).collect::<Vec<_>>(),
        [
            Path::new("/a/target/debug/deps"),
            Path::new("/w/target/debug/deps")
        ],
        "raised at 100,000, not at 80,000; the walk's own row kept"
    );
    assert_eq!(
        mine,
        BTreeSet::from([PathBuf::from("/a/target/debug/deps")])
    );
    let (kept, mine) = fold_outside(
        &raised,
        &mine,
        &counted(&[("/a/target/debug/deps", 76_000)]),
        &[PathBuf::from("/w")],
    );
    assert!(
        kept.contains_key(Path::new("/a/target/debug/deps")),
        "kept at 76,000"
    );
    let (lowered, none) = fold_outside(&kept, &mine, &counted(&[]), &[PathBuf::from("/w")]);
    assert_eq!(
        lowered.keys().map(PathBuf::as_path).collect::<Vec<_>>(),
        [Path::new("/w/target/debug/deps")],
        "a profile no longer counted (its checkout gone) is lowered"
    );
    assert!(none.is_empty());
    let (handed, none) = fold_outside(&kept, &mine, &counted(&[]), &[PathBuf::from("/a")]);
    assert!(
        handed.contains_key(Path::new("/a/target/debug/deps")) && none.is_empty(),
        "a walked root now holds it: its walk's fold owns it"
    );
}

/// The slow tier counts `deps/` entries across each checkout's build profiles while skipping `doc/` and protected folders.
#[test]
fn the_slow_tier_counts_build_profiles_of_the_homes_checkouts() {
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let b = base(&tmp);
    let home = b.join("home");
    for d in [
        "one/target/debug/deps",
        "one/target/release/deps",
        "two/target.noindex/x86_64-unknown-linux-gnu/debug/deps",
        "Documents/three/target/debug/deps",
        "one/target/doc/x/deps",
        "plain/src",
    ] {
        mkdir(&home.join(d));
    }
    std::os::unix::fs::symlink("target.noindex", home.join("two/target")).expect("link");
    for (dir, n) in [
        ("one/target/debug/deps", 3),
        ("one/target/release/deps", 1),
        ("two/target.noindex/x86_64-unknown-linux-gnu/debug/deps", 2),
        ("Documents/three/target/debug/deps", 4),
    ] {
        for i in 0..n {
            write(&home.join(dir).join(format!("f{i}")), 10);
        }
    }
    let policy = policy_for(&home);
    let dirs = outside_build_dirs(&home, &policy);
    assert_eq!(
        dirs,
        vec![home.join("one/target"), home.join("two/target.noindex")],
        "each checkout's build folder; none under the protected folder"
    );
    let mut counts: Vec<(PathBuf, u32)> =
        dirs.iter().flat_map(|t| profile_deps(t, &policy)).collect();
    counts.sort();
    assert_eq!(
        counts,
        vec![
            (home.join("one/target/debug/deps"), 3),
            (home.join("one/target/release/deps"), 1),
            (
                home.join("two/target.noindex/x86_64-unknown-linux-gnu/debug/deps"),
                2
            ),
        ]
    );
    let deps = home.join("one/target/debug/deps");
    let ample = Duration::from_secs(60);
    assert_eq!(profile_executables(&deps, &policy, ample), Some(false));
    let mut stingy = policy.clone();
    stingy.slice = Duration::ZERO;
    assert_eq!(profile_executables(&deps, &stingy, Duration::ZERO), None);
    let bin = deps.join("t-0123");
    write(&bin, 10);
    std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .expect("chmod");
    assert_eq!(profile_executables(&deps, &policy, ample), Some(true));
    assert_eq!(
        profile_executables(
            &home.join("Documents/three/target/debug/deps"),
            &policy,
            ample
        ),
        None,
        "a protected folder is never walked"
    );
}

#[test]
fn a_huge_folder_entry_is_a_well_formed_digest_entry() {
    let (_, h) = huge("/w/target/debug/deps", 104_211);
    let e = huge_entry(&h);
    assert!(e.message().is_ok(), "{:?}", e.message().err());
    let family = aterm_messages::exceptions::Family::HugeDir;
    assert!(e.key().starts_with(&family.prefix()), "{}", e.key());
    assert_eq!(e.family(), family);
    assert_eq!(huge_entry(&h).key(), e.key(), "one folder, one key");
    let (_, other) = huge("/w/other", 104_211);
    assert_ne!(huge_entry(&other).key(), e.key());
    let plain = HugeDir {
        executables: false,
        ..h.clone()
    };
    let p = huge_entry(&plain);
    assert!(p.message().is_ok(), "{:?}", p.message().err());
    assert_ne!(
        format!("{:?}", p.message().ok()),
        format!("{:?}", e.message().ok()),
        "the tccd sentence and the build step only where binaries are"
    );
}

#[test]
fn growth_compares_against_a_walk_a_window_old() {
    let t0 = Instant::now();
    let at = |s: u64| t0 + Duration::from_secs(s);
    let history: VecDeque<(Instant, u64)> =
        VecDeque::from([(at(0), 100), (at(300), 150), (at(700), 400), (at(900), 380)]);
    assert_eq!(
        growth_of(&history, at(900)),
        Some(Growth {
            bytes: 380 - 150,
            over: Duration::from_secs(600)
        })
    );
    let young: VecDeque<(Instant, u64)> = VecDeque::from([(at(0), 100), (at(60), 130)]);
    assert_eq!(
        growth_of(&young, at(60)),
        Some(Growth {
            bytes: 30,
            over: Duration::from_secs(60)
        }),
        "a shorter span is said, not stretched"
    );
    assert_eq!(growth_of(&VecDeque::from([(at(5), 1)]), at(5)), None);
}

#[test]
fn a_root_state_reads_as_its_refusals_word_and_kinds_are_distinct() {
    assert_eq!(RootState::OneLevel.word(), Refusal::TooBroad.word());
    assert_eq!(
        RootState::NotMeasured(Refusal::Protected).word(),
        Refusal::Protected.word()
    );
    let words: BTreeSet<&str> = [
        RootKind::Project,
        RootKind::Folder,
        RootKind::Build,
        RootKind::Own,
        RootKind::OptIn,
    ]
    .iter()
    .map(|k| k.word())
    .collect();
    assert_eq!(
        words,
        BTreeSet::from(["project", "folder", "build", "own", "opt-in"])
    );
}

#[test]
fn a_data_volume_spelling_is_read_as_its_firmlinked_one() {
    assert_eq!(
        firmlinked(Path::new("/System/Volumes/Data/Users//a/w")),
        PathBuf::from("/Users//a/w")
    );
    assert_eq!(
        firmlinked(Path::new("/System/Volumes/Data/private/tmp/x")),
        PathBuf::from("/private/tmp/x")
    );
    assert_eq!(
        firmlinked(Path::new("/System/Volumes/Data/other")),
        PathBuf::from("/System/Volumes/Data/other"),
        "only the firmlinked roots"
    );
}

/// `[sys] disk_roots` expands `~/` against `home`, keeps absolute paths, and drops relative ones.
#[test]
fn the_disk_roots_opt_in_reads_the_home_and_drops_a_relative_folder() {
    let config: crate::app_config::Config = aterm_toml::from_str(
        "[sys]\ndisk_roots = [\"~/Library/Developer/Xcode/DerivedData\", \"/Volumes/Work/x\", \"rel/x\"]\n",
    )
    .expect("valid toml");
    let home = Path::new("/Users//a");
    assert_eq!(
        config.sys_disk_roots(Some(home)),
        vec![
            PathBuf::from("/Users//a/Library/Developer/Xcode/DerivedData"),
            PathBuf::from("/Volumes/Work/x"),
        ]
    );
    assert_eq!(
        config.sys_disk_roots(None),
        vec![PathBuf::from("/Volumes/Work/x")],
        "no home, no `~/`"
    );
    let none: crate::app_config::Config = aterm_toml::from_str("").expect("valid toml");
    assert!(none.sys_disk_roots(Some(home)).is_empty());
}

#[test]
fn a_change_heard_between_reads_owes_the_walker_a_read_and_rest_owes_none() {
    let t0 = Instant::now();
    let mut host = DiskwalkHost::default();
    assert!(host.facts_now(t0, false, true), "the first service reads");
    assert_eq!(
        host.facts_deadline(true),
        None,
        "at rest a read owes no wake"
    );
    let soon = t0 + Duration::from_millis(200);
    assert!(
        !host.facts_now(soon, false, true),
        "a read 200 ms on is not due"
    );
    host.note_activity();
    assert_eq!(
        host.facts_deadline(true),
        Some(t0 + FACTS_EVERY),
        "an OSC 7 report heard between reads is read at the next read's time, not the next wake"
    );
    assert_eq!(
        host.facts_deadline(false),
        None,
        "nothing demanded: no deadline"
    );
    assert!(
        host.facts_now(t0 + FACTS_EVERY, false, true),
        "the deadline's wake reads"
    );
    assert_eq!(
        host.facts_deadline(true),
        None,
        "the read paid what was owed"
    );
}
