// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The walker against trees these tests create (never the owner's home). Unix only: on any
//! other platform every walk is `Refusal::Unsupported`, and the oracle reads Unix metadata
//! (blocks, links, modes).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::refuse::{Deny, Refusal, canonical_for};
use crate::{
    DirId, ListerChoice, Policy, RootSpec, Sliced, Step, Sums, Walk, Walker, diskwalk_size_full,
    entry_count_of, list_one_level,
};

fn p(s: &str) -> PathBuf {
    PathBuf::from(s)
}

// Scratch trees

/// A policy for walking scratch trees under the temp folder: the live mount
/// table (read without asking any file system) and a home the tree is not in.
fn scratch_policy(lister: ListerChoice) -> Policy {
    let mut policy = Policy::new(
        Deny::for_home(Path::new("/Users//diskwalk-test-home"), &[], &[]),
        aterm_sysprobe::volumes::table(),
    );
    policy.lister = lister;
    policy
}

fn write(path: &Path, bytes: usize) {
    std::fs::write(path, vec![7u8; bytes]).expect("write scratch file");
}

/// The tree every walk test reads: nested folders, files of several sizes,
/// an executable, an empty folder, a hard link inside, a hard link out, and
/// a symlink to a folder outside that must not be followed.
fn build(root: &Path, outside: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    for d in ["a/b/c", "a/d", "e", "empty", "x/y/z/w"] {
        std::fs::create_dir_all(root.join(d)).expect("mkdir");
    }
    write(&root.join("top.bin"), 10_000);
    write(&root.join("a/one.bin"), 5_000);
    write(&root.join("a/b/two.bin"), 70_000);
    write(&root.join("a/b/c/three.bin"), 1);
    write(&root.join("a/d/four.bin"), 300_000);
    write(&root.join("e/tool"), 4_096);
    std::fs::set_permissions(root.join("e/tool"), std::fs::Permissions::from_mode(0o755))
        .expect("chmod");
    write(&root.join("x/y/z/w/deep.bin"), 12_345);
    std::fs::hard_link(root.join("a/d/four.bin"), root.join("e/four-again.bin")).expect("link in");
    write(&outside.join("shared.bin"), 40_000);
    std::fs::hard_link(outside.join("shared.bin"), root.join("e/shared.bin")).expect("link out");
    std::fs::create_dir_all(outside.join("big")).expect("mkdir outside");
    write(&outside.join("big/huge.bin"), 1_000_000);
    std::os::unix::fs::symlink(outside.join("big"), root.join("e/to-outside")).expect("symlink");
}

/// The oracle: std over the same tree, no link followed. Σ allocated / links,
/// Σ logical, files, directories (the root not counted).
#[expect(
    clippy::disallowed_methods,
    reason = "the oracle a test holds the walker to reads the scratch tree with std"
)]
fn oracle(root: &Path) -> Sums {
    use std::os::unix::fs::MetadataExt as _;
    let mut s = Sums::default();
    let mut stack = vec![root.to_path_buf()];
    let top = std::fs::symlink_metadata(root).expect("root meta");
    s.alloc_fair += top.blocks() * 512;
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read_dir") {
            let entry = entry.expect("entry");
            let m = std::fs::symlink_metadata(entry.path()).expect("meta");
            if m.is_dir() {
                s.dirs += 1;
                s.alloc_fair += m.blocks() * 512;
                stack.push(entry.path());
            } else {
                s.files += 1;
                s.logical += m.size();
                s.alloc_fair += m.blocks() * 512 / m.nlink().max(1);
                if m.nlink() <= 1 {
                    s.released_floor += m.blocks() * 512;
                }
            }
        }
    }
    s
}

/// A clock that charges `step` per read: the slice's budget is spent by calls,
/// never by how loaded the machine is.
fn tick_clock(step: Duration) -> impl FnMut() -> Instant {
    #[expect(
        clippy::disallowed_methods,
        reason = "an origin for the charged clock; only differences of its readings are used"
    )]
    let base = Instant::now();
    let mut reads = 0u32;
    move || {
        let t = base + step * reads;
        reads += 1;
        t
    }
}

fn walk_all(root: &Path, policy: &Policy) -> Walk {
    let mut walk = Walk::begin(&RootSpec::new(root), policy).expect("begin");
    let mut clock = tick_clock(Duration::from_micros(10));
    while let Step::More { .. } = walk.diskwalk_slice(&mut clock) {}
    assert!(walk.is_done());
    walk
}

fn scratch() -> (aterm_tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let root = tmp.path().join("root");
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&root).expect("mkdir root");
    std::fs::create_dir_all(&outside).expect("mkdir outside");
    build(&root, &outside);
    (tmp, root, outside)
}

#[test]
fn a_walk_sums_what_std_sees_with_either_lister() {
    let (_tmp, root, _outside) = scratch();
    let want = oracle(&root);
    for lister in [ListerChoice::Native, ListerChoice::Portable] {
        let walk = walk_all(&root, &scratch_policy(lister));
        let got = walk.tree().root().subtree;
        assert_eq!(got.alloc_fair, want.alloc_fair, "{lister:?} alloc_fair");
        assert_eq!(got.logical, want.logical, "{lister:?} logical");
        assert_eq!(got.files, want.files, "{lister:?} files");
        assert_eq!(got.dirs, want.dirs, "{lister:?} dirs");
        assert_eq!(got.released_floor, want.released_floor, "{lister:?} floor");
        assert_eq!(got.unreadable + got.gone, 0, "{lister:?} errors");
        assert_eq!(walk.walked(), want.files + want.dirs, "{lister:?} walked");
        assert!(walk.tree().dirs().iter().all(|r| r.complete));
        // The link to the outside folder is a file entry, never entered.
        for i in 0..walk.tree().dirs().len() {
            let name = walk.tree().name(DirId(u32::try_from(i).expect("small")));
            assert!(
                name != b"big" && name != b"to-outside",
                "{lister:?} entered a link"
            );
        }
        assert!(walk.tree().refused().is_empty(), "{lister:?}");
        let e = walk
            .tree()
            .children(DirId::ROOT)
            .into_iter()
            .find(|id| walk.tree().name(*id) == b"e")
            .expect("e recorded");
        let rec = walk.tree().get(e).expect("e");
        assert_eq!(
            rec.direct_entries, 4,
            "{lister:?}: tool, four-again, shared, to-outside"
        );
        assert_eq!(rec.direct_exec, 1, "{lister:?}");
        assert_eq!(walk.partial(), got);
    }
}

#[test]
fn the_native_lister_reads_what_the_portable_one_reads() {
    let (_tmp, root, _outside) = scratch();
    let native = walk_all(&root, &scratch_policy(ListerChoice::Native));
    let portable = walk_all(&root, &scratch_policy(ListerChoice::Portable));
    let key = |w: &Walk| {
        let mut rows: Vec<(PathBuf, u64, Sums, u32, u32)> = w
            .tree()
            .dirs()
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let id = DirId(u32::try_from(i).expect("small"));
                (
                    w.tree().rel_path(id),
                    r.ino,
                    r.subtree,
                    r.direct_entries,
                    r.direct_exec,
                )
            })
            .collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        rows
    };
    assert_eq!(key(&native), key(&portable));
}

#[test]
fn links_out_counts_each_inode_once_and_names_what_is_shared_outside() {
    let (_tmp, root, _outside) = scratch();
    let mut policy = scratch_policy(ListerChoice::Native);
    policy.links_out = true;
    let walk = walk_all(&root, &policy);
    let links = walk.links().expect("link totals");
    assert_eq!(
        links.linked_inodes, 2,
        "four.bin (both links inside) and shared.bin"
    );
    let floor = walk.tree().root().subtree.released_floor;
    #[expect(
        clippy::disallowed_methods,
        reason = "the test's oracle stats its own scratch files"
    )]
    let alloc = |p: &Path| {
        use std::os::unix::fs::MetadataExt as _;
        std::fs::symlink_metadata(p).expect("meta").blocks() * 512
    };
    let four = alloc(&root.join("a/d/four.bin"));
    let shared = alloc(&root.join("e/shared.bin"));
    assert_eq!(links.alloc_first_seen, floor + four + shared);
    assert_eq!(links.shared_out, shared);
    assert_eq!(links.frees_at_most, floor + four);
}

#[test]
fn slices_end_at_the_first_boundary_past_the_budget_and_resume_to_the_same_tree() {
    let (_tmp, root, _outside) = scratch();
    let policy = scratch_policy(ListerChoice::Native);
    let whole = walk_all(&root, &policy);
    let mut walk = Walk::begin(&RootSpec::new(&root), &policy).expect("begin");
    // One millisecond per clock read: a 4 ms slice starts at most four calls.
    let mut clock = tick_clock(Duration::from_millis(1));
    let mut slices = 0;
    let mut last_calls = 0;
    loop {
        let step = walk.diskwalk_slice(&mut clock);
        slices += 1;
        let calls = walk.calls() - last_calls;
        last_calls = walk.calls();
        assert!(calls <= 4, "slice {slices} started {calls} calls");
        if step == Step::Done {
            break;
        }
        assert_eq!(calls, 4, "a slice that is not the last spends its budget");
    }
    assert!(slices > 3, "the tree takes several slices ({slices})");
    assert_eq!(walk.tree().root().subtree, whole.tree().root().subtree);
    assert_eq!(walk.walked(), whole.walked());
}

#[test]
fn a_superseded_ticket_walks_nothing_and_a_cancel_disarms() {
    let (_tmp, root, _outside) = scratch();
    let policy = scratch_policy(ListerChoice::Native);
    let mut walker = Walker::new();
    let mut clock = tick_clock(Duration::from_millis(1));
    let first = walker.start(&RootSpec::new(&root), &policy).expect("start");
    assert!(matches!(
        walker.slice(first, &mut clock),
        Sliced::Step(Step::More { .. })
    ));
    let second = walker
        .start(&RootSpec::new(&root), &policy)
        .expect("restart");
    assert_ne!(first, second);
    let before = walker.walk().expect("walk").calls();
    assert_eq!(walker.slice(first, &mut clock), Sliced::Stale);
    assert_eq!(
        walker.walk().expect("walk").calls(),
        before,
        "a stale ticket made no call"
    );
    assert!(walker.armed());
    walker.cancel();
    assert!(!walker.armed());
    assert_eq!(
        walker.slice(second, &mut clock),
        Sliced::Step(Step::Cancelled)
    );
    assert!(!walker.armed(), "nothing re-arms a cancelled walk");
    // A refused start changes nothing.
    assert!(walker.start(&RootSpec::new("relative"), &policy).is_err());
    assert_eq!(walker.ticket(), Some(second));
}

#[test]
fn deeper_than_the_policy_is_a_labelled_row() {
    let (_tmp, root, _outside) = scratch();
    let mut policy = scratch_policy(ListerChoice::Native);
    policy.max_depth = 2;
    let walk = walk_all(&root, &policy);
    let mut too_deep: Vec<PathBuf> = walk
        .tree()
        .refused()
        .into_iter()
        .filter(|(_, r)| *r == Refusal::TooDeep)
        .map(|(id, _)| walk.tree().rel_path(id))
        .collect();
    too_deep.sort();
    assert_eq!(too_deep, vec![p("a/b/c"), p("x/y/z")]);
}

#[test]
fn a_link_on_the_way_to_a_root_is_never_followed() {
    let (tmp, root, _outside) = scratch();
    let policy = scratch_policy(ListerChoice::Native);
    std::os::unix::fs::symlink(&root, tmp.path().join("via")).expect("symlink");
    assert_eq!(
        Walk::begin(&RootSpec::new(tmp.path().join("via/a")), &policy).err(),
        Some(Refusal::Link)
    );
    assert_eq!(
        Walk::begin(&RootSpec::new(tmp.path().join("via")), &policy).err(),
        Some(Refusal::Link)
    );
    assert_eq!(
        Walk::begin(&RootSpec::new(root.join("top.bin")), &policy).err(),
        Some(Refusal::NotDirectory)
    );
    assert_eq!(
        Walk::begin(&RootSpec::new(root.join("nope")), &policy).err(),
        Some(Refusal::Gone)
    );
}

#[test]
fn a_full_size_answers_a_tree_and_a_file() {
    let (_tmp, root, _outside) = scratch();
    let policy = scratch_policy(ListerChoice::Native);
    let mut clock = tick_clock(Duration::from_micros(10));
    let full = diskwalk_size_full(
        &RootSpec::new(&root),
        &policy,
        Duration::from_secs(60),
        &mut clock,
    )
    .expect("size");
    assert!(full.complete);
    assert_eq!(full.sums.alloc_fair, oracle(&root).alloc_fair);
    let file = diskwalk_size_full(
        &RootSpec::new(root.join("a/d/four.bin")),
        &policy,
        Duration::from_secs(60),
        &mut clock,
    )
    .expect("file");
    assert_eq!(file.sums.files, 1);
    assert_eq!(file.sums.logical, 300_000);
    assert!(
        file.sums.alloc_fair > 0,
        "a file root reads its allocation, never 0"
    );
    // A budget that runs out answers a partial figure.
    let mut slow = tick_clock(Duration::from_millis(1));
    let part = diskwalk_size_full(
        &RootSpec::new(&root),
        &policy,
        Duration::from_millis(5),
        &mut slow,
    )
    .expect("partial");
    assert!(!part.complete);
    assert!(part.walked < full.walked);
}

#[test]
fn entry_counts_come_without_a_listing_where_the_platform_can() {
    let (_tmp, root, _outside) = scratch();
    let policy = scratch_policy(ListerChoice::Native);
    assert_eq!(entry_count_of(&root.join("e"), &policy), Ok(4));
    assert_eq!(entry_count_of(&root.join("empty"), &policy), Ok(0));
}

#[test]
fn a_broad_root_lists_one_level_and_names_a_protected_child_unread() {
    let (tmp, root, _outside) = scratch();
    let base = crate::refuse::canonical(tmp.path()).expect("canonical");
    // Protect `root/a` by override: `root` is then an ancestor of a protected
    // folder, too broad to walk.
    let mut policy = scratch_policy(ListerChoice::Native);
    policy.deny = Deny::for_home(
        Path::new("/Users//diskwalk-test-home"),
        &[base.join("root/a").display().to_string()],
        &[],
    );
    assert_eq!(
        Walk::begin(&RootSpec::new(&root), &policy).err(),
        Some(Refusal::TooBroad)
    );
    let rows = list_one_level(&RootSpec::new(&root), &policy).expect("one level");
    let names: Vec<_> = rows
        .iter()
        .map(|r| r.name.to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["a", "e", "empty", "top.bin", "x"]);
    assert_eq!(rows[0].refused, Some(Refusal::Protected));
    assert_eq!(
        rows[0].direct_entries, None,
        "a protected child is never read"
    );
    if cfg!(target_vendor = "apple") {
        assert_eq!(rows[1].direct_entries, Some(4));
    }
    assert!(rows[3].alloc > 0 && !rows[3].dir);
    assert_eq!(
        Walk::begin(&RootSpec::new(root.join("a/b")), &policy).err(),
        Some(Refusal::Protected)
    );
}

#[test]
fn the_walker_thread_policy_takes_on_a_fresh_thread() {
    let out = std::thread::spawn(|| {
        let out = crate::set_walker_thread_policy();
        #[cfg(target_vendor = "apple")]
        assert_eq!(
            crate::iopol::thread_materialize_policy(),
            Some(crate::iopol::MaterializePolicy::Off)
        );
        out
    })
    .join()
    .expect("thread");
    assert!(out.failed.is_empty(), "{out:?}");
}

/// A clone shares its blocks with its source: each one's private size is
/// what it alone holds, so a cloned pair's private sum sits below its fair
/// allocation (std's copy clones on APFS). The private figure is the lower
/// bound of what deleting the pair frees; the fair allocation the upper.
#[cfg(target_vendor = "apple")]
#[test]
fn private_sizes_see_clones_share_their_blocks() {
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let root = tmp.path().join("clones");
    std::fs::create_dir_all(&root).expect("mkdir");
    write(&root.join("source.bin"), 1 << 20);
    std::fs::copy(root.join("source.bin"), root.join("clone.bin")).expect("copy");
    let mut policy = scratch_policy(ListerChoice::Native);
    policy.private = true;
    let walk = walk_all(&root, &policy);
    let s = walk.tree().root().subtree;
    let private = s.private.expect("private sizes asked for");
    assert!(s.alloc_fair >= 2 << 20, "both files count in full: {s:?}");
    assert!(
        private < s.alloc_fair,
        "the clone pair shares blocks: {s:?}"
    );
    // Without the ask, nothing pretends to know.
    let plain = walk_all(&root, &scratch_policy(ListerChoice::Native));
    assert_eq!(plain.tree().root().subtree.private, None);
}

/// The worktree search finds the nearest `.git` (a folder for a checkout, a
/// file for a linked worktree), reads a `target` link as text, and never
/// looks inside a refused folder or at or above the home.
#[test]
fn marker_looks_find_worktrees_and_links_and_refuse_what_the_deny_set_holds() {
    use crate::look::{Marker, link_target, marker_at, worktree_above};
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let base = canonical_for(tmp.path(), cfg!(target_os = "macos")).expect("absolute");
    let home = base.join("home");
    for d in [
        "home/repo/.git",
        "home/repo/src/deep",
        "home/repo/target.noindex",
        "home/wt/sub",
        "home/plain/x",
        "home/Documents/proj/.git",
    ] {
        std::fs::create_dir_all(base.join(d)).expect("mkdir");
    }
    write(&home.join("wt/.git"), 40);
    std::os::unix::fs::symlink("target.noindex", home.join("repo/target")).expect("link");
    let mut policy = scratch_policy(ListerChoice::Native);
    policy.deny = Deny::for_home(&home, &[], &[]);

    let checkout = worktree_above(&home.join("repo/src/deep"), &policy).expect("checkout");
    assert_eq!(checkout.root, home.join("repo"));
    assert!(!checkout.linked);
    let linked = worktree_above(&home.join("wt/sub"), &policy).expect("worktree");
    assert_eq!(linked.root, home.join("wt"));
    assert!(linked.linked);
    assert_eq!(
        worktree_above(&home.join("plain/x"), &policy),
        None,
        "the search stops below the home"
    );

    assert_eq!(
        marker_at(&home.join("repo"), "target", &policy),
        Ok(Some(Marker::Link))
    );
    assert_eq!(
        link_target(&home.join("repo"), "target", &policy),
        Ok(Some(home.join("repo/target.noindex")))
    );
    assert_eq!(link_target(&home.join("repo"), "src", &policy), Ok(None));
    assert_eq!(marker_at(&home.join("repo"), "absent", &policy), Ok(None));

    // A protected folder is refused on its spelling: nothing in it is asked.
    assert_eq!(worktree_above(&home.join("Documents/proj"), &policy), None);
    assert_eq!(
        marker_at(&home.join("Documents/proj"), ".git", &policy),
        Err(Refusal::Protected)
    );
    assert_eq!(
        marker_at(Path::new("relative"), ".git", &policy),
        Err(Refusal::Relative)
    );
}

#[test]
fn the_build_folder_climb_finds_the_target_folder_then_a_profile_and_never_a_protected_one() {
    use crate::look::{Marker, build_dir_above, markers_at};
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let base = canonical_for(tmp.path(), cfg!(target_os = "macos")).expect("absolute");
    let home = base.join("home");
    for d in [
        "home/repo/target/debug/deps",
        "home/repo/target/debug/incremental/x",
        "home/bare/out/release/build",
        "home/plain/src",
        "home/Documents/proj/target/debug",
    ] {
        std::fs::create_dir_all(base.join(d)).expect("mkdir");
    }
    write(&home.join("repo/target/CACHEDIR.TAG"), 177);
    write(&home.join("repo/target/debug/.cargo-lock"), 0);
    write(&home.join("bare/out/release/.cargo-lock"), 0);
    write(&home.join("Documents/proj/target/CACHEDIR.TAG"), 177);
    let mut policy = scratch_policy(ListerChoice::Native);
    policy.deny = Deny::for_home(&home, &[], &[]);

    // A file deep in a profile anchors at the target folder, past the profile's lock.
    assert_eq!(
        build_dir_above(&home.join("repo/target/debug/incremental/x"), &policy),
        Some(home.join("repo/target"))
    );
    // A tree with only a profile lock anchors at that profile.
    assert_eq!(
        build_dir_above(&home.join("bare/out/release/build"), &policy),
        Some(home.join("bare/out/release"))
    );
    // No marker below the home: none, and the home itself is never asked.
    assert_eq!(build_dir_above(&home.join("plain/src"), &policy), None);
    // A protected folder ends the climb before anything in it is asked.
    assert_eq!(
        build_dir_above(&home.join("Documents/proj/target/debug"), &policy),
        None
    );
    // One open answers every name.
    assert_eq!(
        markers_at(
            &home.join("repo/target"),
            ["CACHEDIR.TAG", "debug", "absent"],
            &policy
        ),
        Ok([Some(Marker::File), Some(Marker::Dir), None])
    );
    assert_eq!(
        markers_at(&home.join("Documents/proj"), ["target"], &policy),
        Err(Refusal::Protected)
    );
}

/// `path`'s device as `stat` reads it, spelled as the walker spells every
/// device ([`crate::list::dev_of`]).
fn stat_dev(path: &Path) -> u64 {
    use std::os::unix::ffi::OsStrExt as _;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("path");
    // SAFETY: `stat` is integers: all-zero is a value.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: a NUL-terminated scratch path and a writable `stat`.
    assert_eq!(unsafe { libc::stat(c.as_ptr(), &raw mut st) }, 0, "stat");
    crate::list::dev_of(st.st_dev)
}

/// A child folder's device, as each lister reads it, is the device `stat`
/// gives the folder itself, which is also the root's: a mismatch makes every
/// subfolder a mount boundary, never entered (Linux's `statx` major and minor
/// against glibc's `makedev`; macOS's `u32` `ATTR_CMN_DEVID` against `fstat`'s
/// `i32`).
#[test]
fn a_child_folders_device_is_the_one_stat_gives_with_either_lister() {
    use std::os::fd::{AsFd as _, FromRawFd as _, OwnedFd};
    use std::os::unix::ffi::OsStrExt as _;
    let (_tmp, root, _) = scratch();
    let want = stat_dev(&root.join("a"));
    assert_eq!(want, stat_dev(&root), "one volume");
    for choice in [ListerChoice::Native, ListerChoice::Portable] {
        let c = std::ffi::CString::new(root.as_os_str().as_bytes()).expect("path");
        // SAFETY: a NUL-terminated scratch path; the descriptor is owned below.
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY) };
        assert!(fd >= 0, "open the scratch root");
        // SAFETY: a fresh descriptor this test owns.
        let dir = unsafe { OwnedFd::from_raw_fd(fd) };
        let mut lister = crate::list::Lister::new(choice, Vec::new(), Vec::new(), false);
        let mut got = None;
        while lister
            .next_batch(dir.as_fd(), &mut |e| {
                if e.name == b"a" {
                    got = Some(e.dev);
                }
            })
            .expect("list")
        {}
        assert_eq!(got, Some(want), "{choice:?}");
    }
    let walk = walk_all(&root, &scratch_policy(ListerChoice::Native));
    assert_eq!(walk.root_dev(), want);
    assert!(
        walk.tree().find(Path::new("a/b/c")).is_some(),
        "subfolders are entered, not boundaries"
    );
}

/// A macOS device number with its top bit set (`/dev` reads -801893391 as an
/// `i32`) is its 32 unsigned bits, as `getattrlistbulk` reports it.
#[cfg(target_vendor = "apple")]
#[test]
fn a_device_number_with_its_top_bit_set_reads_as_its_unsigned_bits() {
    assert_eq!(crate::list::dev_of(-801_893_391), 3_493_073_905);
    assert_eq!(crate::list::dev_of(0x0100_000d), 0x0100_000d);
}

/// `statx`'s major and minor read as glibc's and the kernel's `makedev`
/// encodes them (nvme 259:2 is 0x10302), never `major << 32 | minor`.
#[cfg(target_os = "linux")]
#[test]
fn statx_major_and_minor_read_as_makedev_encodes_them() {
    assert_eq!(crate::list::dev_of_parts(259, 2), 0x1_0302);
    assert_eq!(crate::list::dev_of_parts(8, 1), 0x801);
    assert_eq!(crate::list::dev_of_parts(0, 300), 44 | (256 << 12));
}
