// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The disk walker host against scratch trees these tests create (never the
//! owner's home): root discovery, the report's arithmetic, the §10.8
//! hysteresis and its digest entry, and the thread end to end.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use aterm_diskwalk::{Deny, HugeDir, Policy, Refusal, Sums, canonical};

use super::*;

fn write(path: &Path, bytes: usize) {
    std::fs::write(path, vec![5u8; bytes]).expect("write scratch file");
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

fn kinds(list: &[RootKind]) -> BTreeSet<RootKind> {
    list.iter().copied().collect()
}

/// A scratch home holding a worktree `w` with a `target → target.noindex`
/// link, atpkg's build cache for `w`, a protected folder, and an agent temp
/// root beside it.
fn scratch_home(b: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let home = b.join("home");
    let w = home.join("w");
    let scratch = b.join("claude-test");
    for d in [
        "w/.git",
        "w/crates/x",
        "w/target.noindex/debug",
        "Documents/proj/.git",
    ] {
        std::fs::create_dir_all(home.join(d)).expect("mkdir");
    }
    std::os::unix::fs::symlink("target.noindex", w.join("target")).expect("target link");
    let cache = atpkg::build_cache::build_dir(&atpkg::build_cache::cache_root(&home), &w);
    std::fs::create_dir_all(&cache).expect("build cache");
    std::fs::create_dir_all(scratch.join("-proj/session-1/sub")).expect("scratch");
    write(&w.join("crates/x/lib.rs"), 3_000);
    write(&w.join("target.noindex/debug/bin"), 50_000);
    write(&cache.join("obj"), 20_000);
    (home, w, scratch)
}

#[test]
fn a_tab_claims_its_cwd_processes_worktree_build_output_and_agent_temp_root() {
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let b = base(&tmp);
    let (home, w, scratch) = scratch_home(&b);
    let policy = policy_for(&home);
    let tab = TabFacts {
        key: 7,
        label: "Tab 1".into(),
        shell_pid: 10,
        reported_cwd: Some(w.join("crates/x")),
    };
    // The shell's child (an agent) works in `w`; its child in a session temp folder.
    let procs = [(20, 10), (30, 20), (99, 1)];
    let cwd_of = |pid: u32| match pid {
        20 => Some(w.clone()),
        30 => Some(scratch.join("-proj/session-1/sub")),
        _ => None,
    };
    let reads = ProcReads {
        cwd: &cwd_of,
        target_dir: &|_| None,
        writes: &|_| Vec::new(),
    };
    let claims = discover_tab(&tab, &procs, &reads, &policy, Some(&home), Some(&scratch));
    let by_path: BTreeMap<PathBuf, Claim> =
        claims.into_iter().map(|c| (c.path.clone(), c)).collect();
    let cache = atpkg::build_cache::build_dir(&atpkg::build_cache::cache_root(&home), &w);
    assert_eq!(by_path[&w.join("crates/x")].kinds, kinds(&[RootKind::Cwd]));
    assert_eq!(by_path[&w].kinds, kinds(&[RootKind::Agent, RootKind::Repo]));
    assert!(!by_path[&w].linked_worktree);
    assert_eq!(
        by_path[&w.join("target.noindex")].kinds,
        kinds(&[RootKind::Build]),
        "the target link is read, never followed"
    );
    assert_eq!(by_path[&cache].kinds, kinds(&[RootKind::Build]));
    assert_eq!(
        by_path[&scratch.join("-proj/session-1")].kinds,
        kinds(&[RootKind::Work, RootKind::Scratch])
    );
    assert_eq!(by_path.len(), 5, "{by_path:#?}");

    // A cwd in a protected folder is claimed (and refused when walked), but
    // nothing in it is looked at: no worktree search, no build output.
    let private = TabFacts {
        key: 8,
        label: "Tab 2".into(),
        shell_pid: 11,
        reported_cwd: Some(home.join("Documents/proj")),
    };
    let none = ProcReads {
        cwd: &|_| None,
        target_dir: &|_| None,
        writes: &|_| Vec::new(),
    };
    let claims = discover_tab(&private, &[], &none, &policy, Some(&home), None);
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].kinds, kinds(&[RootKind::Cwd]));
}

/// `writes`: each regular file a descendant holds open for writing anchors at
/// the build folder above it, else its worktree, else its session temp
/// folder, else its folder; a file in a protected folder anchors at that
/// folder unread. `build`: a descendant's `CARGO_TARGET_DIR`, absolute or
/// relative to its cwd, when it is a folder (a link read, never followed).
#[test]
fn a_tab_claims_where_its_processes_write_and_their_cargo_target_dirs() {
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let b = base(&tmp);
    let (home, w, scratch) = scratch_home(&b);
    for d in [
        "w/target.noindex/debug/deps",
        "w/notes/today",
        "elsewhere/logs",
        "shared-target/debug",
        "rel-target",
    ] {
        std::fs::create_dir_all(home.join(d)).expect("mkdir");
    }
    write(&w.join("target.noindex/CACHEDIR.TAG"), 177);
    write(&w.join("target.noindex/debug/.cargo-lock"), 0);
    std::os::unix::fs::symlink("shared-target", home.join("linked-target")).expect("link");
    let policy = policy_for(&home);
    let tab = TabFacts {
        key: 9,
        label: "Tab 3".into(),
        shell_pid: 10,
        reported_cwd: Some(w.clone()),
    };
    let procs = [(20, 10), (30, 20), (40, 20)];
    let cwd_of = |pid: u32| match pid {
        20 | 30 => Some(w.clone()),
        40 => Some(home.clone()),
        _ => None,
    };
    let target_dir = |pid: u32| match pid {
        20 => Some(home.join("linked-target")),
        30 => Some(PathBuf::from("absent-target")),
        40 => Some(PathBuf::from("rel-target")),
        _ => None,
    };
    let writes = |pid: u32| match pid {
        20 => vec![
            w.join("target.noindex/debug/deps/libx.rmeta"),
            w.join("target.noindex/debug/deps/liby.rlib"),
        ],
        30 => vec![
            w.join("notes/today/log.txt"),
            scratch.join("-proj/session-1/sub/out.json"),
            home.join("elsewhere/logs/run.log"),
            home.join("Documents/proj/draft.txt"),
        ],
        _ => Vec::new(),
    };
    let reads = ProcReads {
        cwd: &cwd_of,
        target_dir: &target_dir,
        writes: &writes,
    };
    let claims = discover_tab(&tab, &procs, &reads, &policy, Some(&home), Some(&scratch));
    let by_path: BTreeMap<PathBuf, Claim> =
        claims.into_iter().map(|c| (c.path.clone(), c)).collect();
    // Two files in one profile: one anchor, the target folder (CACHEDIR.TAG)
    // past the profile's `.cargo-lock`, which `w/target` already claims.
    assert_eq!(
        by_path[&w.join("target.noindex")].kinds,
        kinds(&[RootKind::Writes, RootKind::Build])
    );
    // A file in the worktree with no build folder above it: the worktree.
    assert!(
        by_path[&w].kinds.contains(&RootKind::Writes),
        "{by_path:#?}"
    );
    // A file in a session temp folder: that folder.
    assert_eq!(
        by_path[&scratch.join("-proj/session-1")].kinds,
        kinds(&[RootKind::Writes])
    );
    // A file outside any worktree, build or temp folder: its own folder.
    assert_eq!(
        by_path[&home.join("elsewhere/logs")].kinds,
        kinds(&[RootKind::Writes])
    );
    // A protected folder is claimed as written into, never looked in.
    assert_eq!(
        by_path[&home.join("Documents/proj")].kinds,
        kinds(&[RootKind::Writes])
    );
    // CARGO_TARGET_DIR: a link read to its folder; a relative one against the
    // process's cwd; an absent one claims nothing.
    assert_eq!(
        by_path[&home.join("shared-target")].kinds,
        kinds(&[RootKind::Build])
    );
    assert_eq!(
        by_path[&home.join("rel-target")].kinds,
        kinds(&[RootKind::Build])
    );
    assert!(!by_path.contains_key(&w.join("absent-target")));
    assert!(!by_path.contains_key(&home.join("linked-target")));
}

#[test]
fn a_data_volume_spelling_is_read_as_its_firmlinked_one() {
    assert_eq!(
        firmlinked(Path::new("/System/Volumes/Data/Users//a/x.log")),
        PathBuf::from("/Users//a/x.log")
    );
    assert_eq!(
        firmlinked(Path::new("/System/Volumes/Data/usr/local/var/db")),
        PathBuf::from("/usr/local/var/db")
    );
    // Not firmlinked: the data volume's own folders keep their spelling.
    assert_eq!(
        firmlinked(Path::new("/System/Volumes/Data/.Spotlight-V100/x")),
        PathBuf::from("/System/Volumes/Data/.Spotlight-V100/x")
    );
    assert_eq!(
        firmlinked(Path::new("/Users//a/x.log")),
        PathBuf::from("/Users//a/x.log")
    );
}

/// One fact, one wire spelling: a refused root's state reads as the refusal's
/// own word (a too-broad root as `too-broad`, the word a nested row's reason
/// uses), and no two kinds share a word.
#[test]
fn a_root_state_reads_as_its_refusals_word_and_kinds_are_distinct() {
    assert_eq!(RootState::OneLevel.word(), Refusal::TooBroad.word());
    for why in [Refusal::Protected, Refusal::Removable, Refusal::Remote] {
        assert_eq!(RootState::NotMeasured(why).word(), why.word());
    }
    let words: BTreeSet<&str> = [
        RootKind::Cwd,
        RootKind::Agent,
        RootKind::Work,
        RootKind::Writes,
        RootKind::Repo,
        RootKind::Build,
        RootKind::Scratch,
        RootKind::Own,
    ]
    .iter()
    .map(|k| k.word())
    .collect();
    assert_eq!(words.len(), 8);
}

#[test]
fn descendants_are_breadth_first_and_bounded() {
    let mut procs = vec![(2, 1), (3, 2), (4, 1), (5, 3)];
    assert_eq!(descendants(&procs, 1), vec![(2, 1), (4, 1), (3, 2), (5, 3)]);
    for pid in 100..(100 + 2 * MAX_DESCENDANTS as u32) {
        procs.push((pid, 1));
    }
    assert_eq!(descendants(&procs, 1).len(), MAX_DESCENDANTS);
    assert!(descendants(&procs, 9_999).is_empty());
}

#[test]
fn a_session_temp_folder_is_anchored_two_levels_under_its_base() {
    let base = Path::new("/private/tmp/claude-502");
    assert_eq!(
        scratch_anchor(Path::new("/private/tmp/claude-502/-p/s1/a/b"), base),
        Some(PathBuf::from("/private/tmp/claude-502/-p/s1"))
    );
    assert_eq!(
        scratch_anchor(Path::new("/private/tmp/claude-502/-p"), base),
        None
    );
    assert_eq!(scratch_anchor(Path::new("/elsewhere/-p/s1"), base), None);
}

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

fn record(
    path: &str,
    state: RootState,
    c: Option<Complete>,
    id: Option<(u64, u64)>,
) -> Arc<RootRecord> {
    Arc::new(RootRecord {
        path: PathBuf::from(path),
        state,
        id,
        partial: None,
        walked_entries: 0,
        complete: c,
        level: Vec::new(),
        read_from: None,
        asked: None,
    })
}

fn claim_of(path: &str, k: &[RootKind]) -> Claim {
    Claim {
        path: PathBuf::from(path),
        kinds: kinds(k),
        linked_worktree: false,
    }
}

/// Σ exclusive + Σ shared over the outermost rows, a root inside another a
/// pinned child counted once, a root two tabs claim counted in the lower
/// key's tab, two spellings of one folder one row.
#[test]
fn a_report_counts_each_folder_once_and_names_who_shares_it() {
    let huge = HugeDir {
        path: PathBuf::from("/w/target/debug/deps"),
        entries: 104_211,
        executables: true,
        warning: true,
    };
    // A build profile outside every root, raised by the slow tier.
    let outside_row = HugeDir {
        path: PathBuf::from("/old/target/debug/deps"),
        entries: 120_000,
        executables: true,
        warning: true,
    };
    let view = Published {
        tabs: vec![(1, "Tab 1".into()), (2, "Tab 2".into())],
        tab_claims: BTreeMap::from([
            (
                1,
                vec![
                    claim_of("/w", &[RootKind::Cwd, RootKind::Repo]),
                    claim_of("/w/target", &[RootKind::Build]),
                    claim_of("/tmp/s", &[RootKind::Scratch]),
                    claim_of("/alias", &[RootKind::Work]),
                ],
            ),
            (2, vec![claim_of("/w", &[RootKind::Cwd])]),
        ]),
        own_claims: vec![claim_of("/state", &[RootKind::Own])],
        records: BTreeMap::from([
            (
                PathBuf::from("/w"),
                record(
                    "/w",
                    RootState::Complete,
                    Some(complete(
                        1_000,
                        vec![huge.clone()],
                        Some(Growth {
                            bytes: 40,
                            over: Duration::from_secs(600),
                        }),
                    )),
                    Some((1, 10)),
                ),
            ),
            (
                PathBuf::from("/w/target"),
                record(
                    "/w/target",
                    RootState::Complete,
                    Some(complete(700, vec![huge.clone()], None)),
                    None,
                ),
            ),
            (
                PathBuf::from("/tmp/s"),
                record(
                    "/tmp/s",
                    RootState::Complete,
                    Some(complete(
                        300,
                        Vec::new(),
                        Some(Growth {
                            bytes: -10,
                            over: Duration::from_secs(300),
                        }),
                    )),
                    Some((1, 20)),
                ),
            ),
            (
                PathBuf::from("/alias"),
                // The same folder as `/tmp/s` (one `(dev, ino)`): the same figures.
                record(
                    "/alias",
                    RootState::Complete,
                    Some(complete(
                        300,
                        Vec::new(),
                        Some(Growth {
                            bytes: -10,
                            over: Duration::from_secs(300),
                        }),
                    )),
                    Some((1, 20)),
                ),
            ),
            (
                PathBuf::from("/state"),
                record("/state", RootState::Walking, None, None),
            ),
        ]),
        huge: BTreeMap::from([(outside_row.path.clone(), outside_row.clone())]),
        outside: BTreeSet::from([outside_row.path.clone()]),
        ..Published::default()
    };
    let report = |scope| build_report(scope, &view, WalkerState::Running);
    let tab1 = report(DiskScope::Tab(1));
    assert_eq!(tab1.label.as_deref(), Some("Tab 1"));
    assert!(!tab1.unknown_tab);
    let paths: Vec<&Path> = tab1.roots.iter().map(|r| r.claim.path.as_path()).collect();
    assert_eq!(
        paths,
        [Path::new("/w"), Path::new("/w/target"), Path::new("/alias")],
        "largest first, the pinned child after its root, two spellings one row"
    );
    assert_eq!(tab1.roots[1].inside, Some(1));
    assert_eq!(tab1.roots[0].shared_with, vec![2]);
    assert_eq!(tab1.roots[0].counted_in, Some(1));
    assert_eq!(
        tab1.roots[2].claim.kinds,
        kinds(&[RootKind::Work, RootKind::Scratch])
    );
    assert_eq!(tab1.shared_bytes, 1_000);
    assert_eq!(
        tab1.exclusive_bytes, 300,
        "the pinned child is not added again"
    );
    assert_eq!(tab1.warnings, vec![huge.clone()], "one warning per folder");
    assert_eq!(
        tab1.growth,
        Some(Growth {
            bytes: 30,
            over: Duration::from_secs(300)
        })
    );
    assert!(!tab1.walking);

    let tab2 = report(DiskScope::Tab(2));
    assert_eq!(tab2.roots.len(), 1);
    assert_eq!(tab2.roots[0].shared_with, vec![1]);
    assert_eq!(tab2.roots[0].counted_in, Some(1));
    assert_eq!(tab2.exclusive_bytes, 0);

    let all = report(DiskScope::Aterm);
    assert_eq!(
        all.warnings,
        vec![outside_row.clone(), huge.clone()],
        "aterm's scope also names the profiles outside every root, most entries first"
    );
    assert_eq!(all.roots.len(), 4);
    assert_eq!(all.total_bytes, 1_300);
    assert_eq!(all.shared_bytes, 1_000, "the folder both panes claim, once");
    assert_eq!(all.exclusive_bytes, 300);
    assert_eq!(all.roots[0].shared_with, vec![1, 2]);
    assert!(all.walking, "aterm's own folder is still walking");
    assert_eq!(
        tab1.exclusive_bytes + tab2.exclusive_bytes + all.shared_bytes,
        all.total_bytes,
        "Σ pane-exclusive + shared = aterm distinct (aterm's own folder holds 0 yet)"
    );

    let gone = report(DiskScope::Tab(9));
    assert!(gone.unknown_tab && gone.roots.is_empty());
}

/// The balance across panes when one pane's root lies inside another's: the
/// inner folder's bytes are shared, counted once in the aterm scope and in
/// neither pane's exclusive total, so Σ pane-exclusive + shared = aterm
/// distinct (design §10.4). NEGATIVE CONTROL: with the inner claim gone, no
/// byte is shared.
#[test]
fn nested_roots_of_two_panes_share_the_inner_folder_once() {
    let complete_rec = |path: &str, alloc: u64, id: Option<(u64, u64)>| {
        (
            PathBuf::from(path),
            record(
                path,
                RootState::Complete,
                Some(complete(alloc, Vec::new(), None)),
                id,
            ),
        )
    };
    let mut view = Published {
        tabs: vec![
            (1, "Tab 1".into()),
            (2, "Tab 2".into()),
            (3, "Tab 3".into()),
        ],
        tab_claims: BTreeMap::from([
            (1, vec![claim_of("/a", &[RootKind::Repo])]),
            (2, vec![claim_of("/a/c", &[RootKind::Cwd])]),
            (
                3,
                vec![
                    claim_of("/b", &[RootKind::Cwd]),
                    claim_of("/a/c/d", &[RootKind::Work]),
                ],
            ),
        ]),
        records: BTreeMap::from([
            complete_rec("/a", 100, Some((1, 1))),
            complete_rec("/a/c", 30, None),
            complete_rec("/a/c/d", 10, None),
            complete_rec("/b", 7, Some((1, 2))),
        ]),
        ..Published::default()
    };
    let report = |v: &Published, scope| build_report(scope, v, WalkerState::Running);
    let (t1, t2, t3) = (
        report(&view, DiskScope::Tab(1)),
        report(&view, DiskScope::Tab(2)),
        report(&view, DiskScope::Tab(3)),
    );
    let all = report(&view, DiskScope::Aterm);
    assert_eq!((t1.exclusive_bytes, t1.shared_bytes), (70, 30));
    assert_eq!(
        t1.roots[0].shared_with,
        vec![2, 3],
        "panes claiming folders in it"
    );
    assert_eq!((t2.exclusive_bytes, t2.shared_bytes), (0, 30));
    assert_eq!(t2.roots[0].shared_with, vec![1, 3]);
    assert_eq!(t2.roots[0].counted_in, Some(1), "pane 1 claims it whole");
    assert_eq!((t3.exclusive_bytes, t3.shared_bytes), (7, 10));
    assert_eq!((all.total_bytes, all.shared_bytes), (107, 30));
    assert_eq!(
        t1.exclusive_bytes + t2.exclusive_bytes + t3.exclusive_bytes + all.shared_bytes,
        all.total_bytes
    );
    view.tab_claims.remove(&2);
    view.tab_claims
        .insert(3, vec![claim_of("/b", &[RootKind::Cwd])]);
    let alone = report(&view, DiskScope::Tab(1));
    assert_eq!((alone.exclusive_bytes, alone.shared_bytes), (100, 0));
    assert!(alone.roots[0].shared_with.is_empty());
    assert_eq!(report(&view, DiskScope::Aterm).shared_bytes, 0);
}

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

/// Raised at 100,000, kept down to 75,000, lowered under it; a walk of one
/// root never touches another root's raised folders.
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

/// The slow tier's fold: a counted profile follows the walk's hysteresis;
/// one it raised and no longer counts is lowered, unless a walked root now
/// holds it; a walk's own raised folders are never touched.
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

/// The slow tier over a scratch home: each checkout's build folder (a link to
/// `target.noindex` read, never followed), each profile's `deps/` counted,
/// a cross build's too, never a `doc` folder's, and nothing under a protected
/// folder looked at.
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
        std::fs::create_dir_all(home.join(d)).expect("mkdir");
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
    // A profile at the note line is walked for its executables: a whole
    // listing without one says no.
    let deps = home.join("one/target/debug/deps");
    let ample = Duration::from_secs(60);
    assert_eq!(profile_executables(&deps, &policy, ample), Some(false));
    // A walk the budget cuts before it lists the whole folder knows nothing,
    // so the slow tier keeps no answer and asks again (on a loaded machine a
    // 100,000-entry `deps/` outlasted the budget, and the cut walk's "none"
    // was kept for an hour).
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
    // A folder with no executables is no build folder: its step says so.
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

/// The thread end to end on its own board, the loop played by the test: a
/// tab demanded with [`Detail::Frees`] has its worktree walked with private
/// sizes and link totals, its build output inside the worktree is read from
/// that walk, not walked again, and a protected cwd is a named refusal. The
/// walker shares no lock with the reader: what it measured arrives by message.
#[test]
fn the_walker_thread_sizes_a_demanded_tab_and_reads_nested_roots_from_one_walk() {
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let b = base(&tmp);
    let (home, w, _scratch) = scratch_home(&b);
    let shared: &'static Shared = Box::leak(Box::new(Shared::new()));
    let from_walker = spawn_walker(shared, None).expect("the walker starts");
    assert_eq!(shared.walker_state(), WalkerState::Running);
    shared.send(ToWalker::Facts(Facts {
        tabs: vec![
            TabFacts {
                key: 1,
                label: "Tab 1".into(),
                // No such process: its descendants and cwd read as nothing.
                shell_pid: u32::MAX - 7,
                reported_cwd: Some(w.clone()),
            },
            TabFacts {
                key: 2,
                label: "Tab 2".into(),
                shell_pid: u32::MAX - 8,
                reported_cwd: Some(home.join("Documents/proj")),
            },
        ],
        home: Some(home.clone()),
        ..Facts::default()
    }));
    shared.demand(DiskScope::Tab(1), Detail::Frees, Instant::now());
    shared.demand(DiskScope::Tab(2), Detail::Sizes, Instant::now());
    assert!(shared.demanded(Instant::now()));
    // A hang detector, not a latency budget: the walk must end.
    let deadline = Instant::now() + Duration::from_secs(60);
    let (one, two) = loop {
        shared.absorb(&from_walker);
        let one = shared.report(DiskScope::Tab(1));
        let two = shared.report(DiskScope::Tab(2));
        let settled = |r: &DiskReport| !r.walking && !r.roots.is_empty();
        if settled(&one) && settled(&two) {
            break (one, two);
        }
        assert!(
            Instant::now() < deadline,
            "the walker never settled: {one:#?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    shared.send(ToWalker::Stop);
    let row = |r: &DiskReport, p: &Path| {
        r.roots
            .iter()
            .find(|row| row.claim.path == p)
            .cloned()
            .unwrap_or_else(|| panic!("no row for {}: {r:#?}", p.display()))
    };
    let repo = row(&one, &w);
    assert_eq!(repo.claim.kinds, kinds(&[RootKind::Cwd, RootKind::Repo]));
    assert_eq!(repo.record.state, RootState::Complete);
    let c = repo.record.complete.as_ref().expect("complete");
    assert!(c.sums.alloc_fair >= 53_000, "{c:?}");
    assert_eq!(c.detail, Detail::Frees);
    assert!(c.links.is_some(), "a Frees walk keeps its link totals");
    #[cfg(target_vendor = "apple")]
    assert!(
        c.sums.private.is_some(),
        "and private sizes where the OS has them"
    );
    let build = row(&one, &w.join("target.noindex"));
    assert_eq!(build.inside, Some(repo.n));
    assert_eq!(
        build.record.read_from.as_deref(),
        Some(w.as_path()),
        "read from the worktree's walk, not walked twice"
    );
    let build_sums = build.record.complete.as_ref().expect("derived").sums;
    assert!(build_sums.alloc_fair >= 50_000 && build_sums.alloc_fair < c.sums.alloc_fair);
    let cache = atpkg::build_cache::build_dir(&atpkg::build_cache::cache_root(&home), &w);
    let cache_row = row(&one, &cache);
    assert_eq!(cache_row.record.state, RootState::Complete);
    assert_eq!(cache_row.inside, None);
    assert_eq!(
        one.exclusive_bytes,
        c.sums.alloc_fair + cache_row.alloc(),
        "the pinned build folder is counted once, inside the worktree"
    );
    assert_eq!(one.total_bytes, one.exclusive_bytes);
    let private = row(&two, &home.join("Documents/proj"));
    assert_eq!(
        private.record.state,
        RootState::NotMeasured(Refusal::Protected)
    );
}

/// The lifetime law (§10.4): a scope demanded anew has its roots walked at
/// once; a root that appears later is walked at its second consecutive
/// discovery (one sighting is a `cd`), a missed discovery restarts the count,
/// and a root no discovery names is kept a minute, then retired.
#[test]
fn a_root_is_walked_at_its_second_sighting_and_retired_a_minute_after_its_last() {
    let t0 = Instant::now();
    let at = |s: u64| t0 + Duration::from_secs(s);
    let set = |ps: &[&str]| ps.iter().map(PathBuf::from).collect::<BTreeSet<_>>();
    let (w, x) = (Path::new("/w"), Path::new("/tmp/x"));
    let mut life = BTreeMap::new();
    assert!(age_roots(&mut life, &set(&["/w"]), &set(&["/w"]), 1, at(0)).is_empty());
    assert!(life[w].promoted, "a newly demanded scope's root: at once");
    age_roots(&mut life, &set(&["/w", "/tmp/x"]), &set(&[]), 2, at(5));
    assert!(!life[x].promoted, "one sighting");
    age_roots(&mut life, &set(&["/w"]), &set(&[]), 3, at(10));
    age_roots(&mut life, &set(&["/w", "/tmp/x"]), &set(&[]), 4, at(15));
    assert!(!life[x].promoted, "a missed discovery restarts the count");
    age_roots(&mut life, &set(&["/w", "/tmp/x"]), &set(&[]), 5, at(20));
    assert!(life[x].promoted, "two in a row");
    assert!(age_roots(&mut life, &set(&["/w"]), &set(&[]), 6, at(79)).is_empty());
    assert!(life[x].promoted, "kept while unnamed for under a minute");
    assert_eq!(
        age_roots(&mut life, &set(&["/w"]), &set(&[]), 7, at(80)),
        vec![PathBuf::from("/tmp/x")]
    );
    assert!(!life.contains_key(x) && life.contains_key(w));
}

/// A complete walk is owed another at 30 s, or at twenty times its own
/// duration when that is later: at most a 5 % duty on a root left demanded.
#[test]
fn a_walk_is_walked_again_after_thirty_seconds_or_twenty_times_its_cost() {
    let mut c = complete(1, Vec::new(), None);
    c.took = Duration::from_millis(200);
    assert_eq!(c.rewalk_at(), c.at + REWALK_AFTER);
    c.took = Duration::from_secs(4);
    assert_eq!(c.rewalk_at(), c.at + Duration::from_secs(80));
}

/// The walk owed next: a root holding a `writes` root first (outermost, so
/// the folder inside it is read from its walk), then the rest outermost
/// first; a root inside one still to walk waits; a complete walk is owed
/// again at its re-walk time, or now when a scope wants what frees it.
#[test]
fn the_next_walk_holds_a_writes_root_first_and_a_nested_root_waits() {
    let now = Instant::now();
    let plan: Vec<PathBuf> = ["/a", "/z", "/q/out", "/z/t"]
        .iter()
        .map(PathBuf::from)
        .collect();
    let mut records: BTreeMap<PathBuf, Arc<RootRecord>> = BTreeMap::new();
    let writes = BTreeSet::from([PathBuf::from("/z/t")]);
    let none = BTreeSet::new();
    let pick = |r: &BTreeMap<PathBuf, Arc<RootRecord>>, w: &BTreeSet<PathBuf>, f| {
        pick_root(&plan, r, w, f, now)
    };
    assert_eq!(pick(&records, &writes, &none), Some(PathBuf::from("/z")));
    records.insert(
        PathBuf::from("/z"),
        record("/z", RootState::Walking, None, None),
    );
    assert_eq!(
        pick(&records, &writes, &none),
        Some(PathBuf::from("/a")),
        "/z/t waits for /z's walk; then outermost first"
    );
    let deeper_writes = BTreeSet::from([PathBuf::from("/q/out")]);
    assert_eq!(
        pick(&records, &deeper_writes, &none),
        Some(PathBuf::from("/q/out")),
        "a writes root before a shallower quiet one"
    );
    let done = |path: &str, ago: u64| {
        let mut c = complete(1, Vec::new(), None);
        c.at = now - Duration::from_secs(ago);
        (
            PathBuf::from(path),
            record(path, RootState::Complete, Some(c), None),
        )
    };
    let mut walked: BTreeMap<PathBuf, Arc<RootRecord>> = BTreeMap::from([
        done("/a", 10),
        done("/z", 40),
        done("/q/out", 5),
        done("/z/t", 5),
    ]);
    assert_eq!(
        pick(&walked, &none, &none),
        Some(PathBuf::from("/z")),
        "past 30 s"
    );
    walked.extend([done("/z", 10)]);
    assert_eq!(pick(&walked, &none, &none), None, "nothing due");
    let frees = BTreeSet::from([PathBuf::from("/a")]);
    assert_eq!(
        pick(&walked, &none, &frees),
        Some(PathBuf::from("/a")),
        "a sizes-only walk is owed now when its frees are asked"
    );
}

/// `[sys] disk_roots` (O20) reads `~/` against the home, keeps an absolute
/// folder and drops a relative one; with no `[sys]` table nothing is opted in.
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
