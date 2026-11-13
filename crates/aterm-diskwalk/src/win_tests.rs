// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Windows walker against scratch trees the tests create (a junction, hard links, a folder
//! whose listing is denied, a path past `MAX_PATH`), and the path test against Windows spellings.

use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::fs::OpenOptionsExt as _;
use std::os::windows::io::AsRawHandle as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::look::{Marker, Worktree, build_dir_above, link_target, marker_at, worktree_above};
use crate::refuse::{Deny, Refusal, canonical, refuse};
use crate::win::{LinkReads, Lister};
use crate::{
    DirId, Policy, RootSpec, Sliced, Step, Sums, Walk, Walker, directory_identity,
    diskwalk_size_full, entry_count_of, list_one_level,
};

/// A home no scratch tree lies under or beside.
const HOME: &str = r"C:\diskwalk-test-homes\me";
const ERROR_ACCESS_DENIED: i32 = 5;
const FSCTL_SET_REPARSE_POINT: u32 = 0x0009_00A4;
const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

#[repr(C)]
struct SecurityAttributes {
    length: u32,
    security_descriptor: *mut c_void,
    inherit_handle: i32,
}

// SAFETY (declaration): advapi32 exports this with this signature on every supported Windows.
#[link(name = "advapi32")]
unsafe extern "system" {
    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        string_security_descriptor: *const u16,
        revision: u32,
        security_descriptor: *mut *mut c_void,
        size: *mut u32,
    ) -> i32;
}

// SAFETY (declaration): kernel32 exports these with these signatures on every supported Windows.
#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateDirectoryW(path_name: *const u16, attributes: *const SecurityAttributes) -> i32;
    fn LocalFree(mem: *mut c_void) -> *mut c_void;
}

fn policy() -> Policy {
    Policy::new(
        Deny::for_home(Path::new(HOME), &[], &[]),
        aterm_sysprobe::volumes::table(),
    )
}

fn write(path: &Path, bytes: usize) {
    std::fs::write(path, vec![7u8; bytes]).expect("write scratch file");
}

fn wide_z(s: &std::ffi::OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// Make `link`, a new directory, a junction to the directory `target`: the reparse data
/// `mklink /J` writes, which needs no privilege on a directory this account owns.
fn make_junction(link: &Path, target: &Path) {
    let target = canonical(target).expect("an absolute target");
    let print: Vec<u16> = target.as_os_str().encode_wide().collect();
    let substitute: Vec<u16> = r"\??\"
        .encode_utf16()
        .chain(print.iter().copied())
        .collect();
    let bytes = |n: usize| u16::try_from(n).expect("a short name");
    let names = (substitute.len() + 1 + print.len() + 1) * 2;
    let fields = [
        bytes(8 + names),
        0,
        0,
        bytes(substitute.len() * 2),
        bytes((substitute.len() + 1) * 2),
        bytes(print.len() * 2),
    ];
    let mut data: Vec<u8> = Vec::new();
    data.extend(IO_REPARSE_TAG_MOUNT_POINT.to_le_bytes());
    for unit in fields
        .iter()
        .chain(&substitute)
        .chain(&[0])
        .chain(&print)
        .chain(&[0])
    {
        data.extend(unit.to_le_bytes());
    }
    std::fs::create_dir(link).expect("mkdir junction");
    let dir = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(link)
        .expect("open junction");
    let mut returned = 0u32;
    // SAFETY: `dir` is a live handle open for writing; the input is `data`, alive for the call
    // and exactly its length; no output buffer; a synchronous call (no OVERLAPPED).
    let ok = unsafe {
        crate::win::DeviceIoControl(
            dir.as_raw_handle(),
            FSCTL_SET_REPARSE_POINT,
            data.as_ptr().cast(),
            u32::try_from(data.len()).expect("small"),
            std::ptr::null_mut(),
            0,
            &raw mut returned,
            std::ptr::null_mut(),
        )
    };
    assert_ne!(
        ok,
        0,
        "set the junction: {}",
        std::io::Error::last_os_error()
    );
}

/// A directory nobody may list, removed on drop (its delete is still allowed).
struct Locked(PathBuf);

impl Locked {
    /// `D:P(D;;0x1;;;WD)(A;;FA;;;WD)`: a protected DACL that denies everyone
    /// `FILE_LIST_DIRECTORY` and allows everything else.
    fn new(path: &Path) -> Self {
        let sddl = wide_z(std::ffi::OsStr::new("D:P(D;;0x1;;;WD)(A;;FA;;;WD)"));
        let mut descriptor: *mut c_void = std::ptr::null_mut();
        // SAFETY: `sddl` is NUL-terminated; `descriptor` receives a `LocalAlloc`ed descriptor
        // this function frees; the size is not wanted.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &raw mut descriptor,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(ok, 0, "sddl: {}", std::io::Error::last_os_error());
        let attributes = SecurityAttributes {
            length: u32::try_from(std::mem::size_of::<SecurityAttributes>()).expect("small"),
            security_descriptor: descriptor,
            inherit_handle: 0,
        };
        let name = wide_z(path.as_os_str());
        // SAFETY: `name` is NUL-terminated and `attributes` names a live descriptor, both alive
        // for the call.
        let made = unsafe { CreateDirectoryW(name.as_ptr(), &raw const attributes) };
        let why = std::io::Error::last_os_error();
        // SAFETY: `descriptor` came from the conversion above and is not used again.
        unsafe { LocalFree(descriptor) };
        assert_ne!(made, 0, "mkdir locked: {why}");
        Self(path.to_path_buf())
    }
}

impl Drop for Locked {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.0);
    }
}

fn build(root: &Path, outside: &Path) {
    for d in ["a/b/c", "a/d", "e", "empty", "x/y/z/w"] {
        std::fs::create_dir_all(root.join(d)).expect("mkdir");
    }
    write(&root.join("top.bin"), 10_000);
    write(&root.join("a/one.bin"), 5_000);
    write(&root.join("a/b/two.bin"), 70_000);
    write(&root.join("a/b/c/three.bin"), 1);
    write(&root.join("a/d/four.bin"), 300_000);
    write(&root.join("e/tool"), 4_096);
    write(&root.join("x/y/z/w/deep.bin"), 12_345);
    std::fs::hard_link(root.join("a/d/four.bin"), root.join("e/four-again.bin")).expect("link in");
    write(&outside.join("shared.bin"), 40_000);
    std::fs::hard_link(outside.join("shared.bin"), root.join("e/shared.bin")).expect("link out");
    std::fs::create_dir_all(outside.join("big/sub")).expect("mkdir outside");
    write(&outside.join("big/huge.bin"), 1_000_000);
    make_junction(&root.join("e/to-outside"), &outside.join("big"));
}

/// What `build` wrote under the root, the junction counted as the one entry it is.
const FILES: u64 = 10;
const DIRS: u64 = 10;
const LOGICAL: u64 = 10_000 + 5_000 + 70_000 + 1 + 300_000 + 4_096 + 12_345 + 300_000 + 40_000;

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

/// [`walk_all`] of a tree a walk has been through once, for a test that compares two walks of
/// it. NTFS brings a folder's record in its parent's index up to date when a handle to it closes,
/// so a folder changed since it was last closed (one a test just filled) is listed with its older
/// modification time until a walk has opened and closed it: the first walk of a tree made within
/// the last second can read another second than the next.
fn settled_walk(root: &Path, policy: &Policy) -> Walk {
    let _ = walk_all(root, policy);
    walk_all(root, policy)
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

fn size_of_file(path: &Path, policy: &Policy) -> crate::SizeFull {
    diskwalk_size_full(
        &RootSpec::new(path),
        policy,
        Duration::from_secs(60),
        &mut tick_clock(Duration::from_micros(10)),
    )
    .expect("a file root")
}

fn names(walk: &Walk) -> Vec<String> {
    (0..walk.tree().dirs().len())
        .map(|i| {
            let id = DirId(u32::try_from(i).expect("small"));
            String::from_utf8_lossy(walk.tree().name(id)).into_owned()
        })
        .collect()
}

#[test]
fn windows_spellings_read_as_one_plain_drive_path() {
    let c = |s: &str| canonical(Path::new(s));
    assert_eq!(c(r"c:\Users\a"), Some(PathBuf::from(r"C:\Users\a")));
    assert_eq!(c(r"\\?\C:\Users\a\"), Some(PathBuf::from(r"C:\Users\a")));
    assert_eq!(c(r"C:\"), Some(PathBuf::from(r"C:\")));
    assert_eq!(c(r"C:/Users//a"), Some(PathBuf::from(r"C:\Users\a")));
    assert_eq!(
        c(r"\\server\share\x"),
        Some(PathBuf::from(r"\\server\share\x"))
    );
    assert_eq!(
        c(r"\\?\UNC\server\share\x"),
        Some(PathBuf::from(r"\\server\share\x"))
    );
    for odd in [
        r"C:Users",
        r"Users\a",
        r"\Users\a",
        r"C:\Users\..\a",
        r"\\?\C:\Users\..\a",
        r"\\.\C:\Users",
        r"\\?\Volume{6140fc3b-0000-0000-0000-100000000000}\Users",
    ] {
        assert_eq!(c(odd), None, "{odd}");
    }
}

#[test]
fn the_path_test_reads_windows_roots_homes_and_shares() {
    let policy = policy();
    let refused = |s: &str| refuse(None, Path::new(s), &policy.mounts, &policy.deny);
    assert_eq!(refused(r"C:\"), Some(Refusal::TooBroad));
    assert_eq!(refused(HOME), Some(Refusal::TooBroad));
    assert_eq!(refused(r"c:\DISKWALK-TEST-HOMES"), Some(Refusal::TooBroad));
    assert_eq!(
        refused(r"C:\diskwalk-test-homes\other\src"),
        Some(Refusal::OtherUser)
    );
    assert_eq!(refused(r"src\x"), Some(Refusal::Relative));
    assert_eq!(refused(r"\\server\share\src"), Some(Refusal::Remote));
    // No folder of a Windows home prompts: none is denied by name.
    for sub in ["Documents", "Desktop", "Downloads", "Library"] {
        assert_eq!(
            policy.deny.denies(&Path::new(HOME).join(sub).join("x")),
            None,
            "{sub}"
        );
    }
    for (entry, answer) in [
        (
            "Walk::begin",
            Walk::begin(&RootSpec::new(r"src\x"), &policy).err(),
        ),
        (
            "entry_count_of",
            entry_count_of(Path::new("src"), &policy).err(),
        ),
        (
            "marker_at",
            marker_at(Path::new("src"), ".git", &policy).err(),
        ),
    ] {
        assert_eq!(answer, Some(Refusal::Relative), "{entry}");
    }
}

#[test]
fn a_refused_root_is_never_opened() {
    let policy = policy();
    let (answer, seen) = crate::touched::record(|| {
        Walk::begin(&RootSpec::new(r"C:\diskwalk-test-homes\other\src"), &policy).err()
    });
    assert_eq!(answer, Some(Refusal::OtherUser));
    assert_eq!(seen, Vec::<PathBuf>::new());
}

#[test]
fn a_walk_sums_the_tree_follows_no_junction_and_shares_a_hard_link_out() {
    let (_tmp, root, outside) = scratch();
    let mut policy = policy();
    policy.links_out = true;
    let walk = walk_all(&root, &policy);
    let got = walk.tree().root().subtree;
    assert_eq!(got.files, FILES);
    assert_eq!(got.dirs, DIRS);
    assert_eq!(got.logical, LOGICAL);
    assert_eq!(got.unreadable + got.gone, 0);
    assert_eq!(got.dataless_logical, 0);
    assert_eq!(walk.walked(), FILES + DIRS);
    assert!(walk.tree().dirs().iter().all(|r| r.complete));
    assert!(
        !names(&walk)
            .iter()
            .any(|n| n == "big" || n == "sub" || n == "to-outside"),
        "entered a junction: {:?}",
        names(&walk)
    );
    assert!(got.newest_mtime_s > 1_600_000_000, "{}", got.newest_mtime_s);

    // Each file measured alone: its whole allocation, however many links it has.
    let four = size_of_file(&root.join("a/d/four.bin"), &policy)
        .links
        .expect("links_out")
        .alloc_first_seen;
    let shared = size_of_file(&outside.join("shared.bin"), &policy)
        .links
        .expect("links_out")
        .alloc_first_seen;
    assert!(four > 0 && shared > 0, "four={four} shared={shared}");
    let singles: u64 = [
        "top.bin",
        "a/one.bin",
        "a/b/two.bin",
        "a/b/c/three.bin",
        "e/tool",
        "x/y/z/w/deep.bin",
    ]
    .iter()
    .map(|f| {
        let one = size_of_file(&root.join(f), &policy);
        assert_eq!(one.sums.released_floor, one.sums.alloc_fair, "{f}");
        one.sums.alloc_fair
    })
    .sum();
    assert_eq!(got.released_floor, singles);
    // Both links of `four` are inside: its allocation once. One of `shared`'s two is: half.
    let folders: u64 = walk
        .tree()
        .dirs()
        .iter()
        .map(|r| r.own.alloc_fair)
        .sum::<u64>()
        - singles
        - four / 2 * 2
        - shared / 2;
    assert_eq!(
        got.alloc_fair,
        singles + four / 2 * 2 + shared / 2 + folders
    );
    assert!(folders < four / 2, "a link counted twice: {folders}");

    let links = walk.links().expect("links_out");
    assert_eq!(links.linked_inodes, 2);
    assert_eq!(links.alloc_first_seen, singles + four + shared);
    assert_eq!(links.shared_out, shared);
    assert_eq!(links.frees_at_most, singles + four);
}

#[test]
fn a_sizing_walk_reads_the_link_count_of_big_files_only() {
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let root = tmp.path().join("root");
    std::fs::create_dir_all(root.join("sub")).expect("mkdir");
    let floor = usize::try_from(crate::walk::imp::LINK_READ_FLOOR).expect("fits");
    write(&root.join("big.bin"), 2 * floor);
    write(&root.join("small.bin"), 10_000);
    std::fs::hard_link(root.join("big.bin"), root.join("sub/big-again.bin")).expect("link");
    std::fs::hard_link(root.join("small.bin"), root.join("sub/small-again.bin")).expect("link");
    let quick_policy = policy();
    let mut exact_policy = policy();
    exact_policy.links_out = true;
    let quick = walk_all(&root, &quick_policy).tree().root().subtree;
    let exact_walk = walk_all(&root, &exact_policy);
    let exact = exact_walk.tree().root().subtree;
    assert_eq!((quick.files, quick.dirs), (4, 1));
    assert_eq!((exact.files, exact.dirs), (4, 1));
    assert_eq!(quick.logical, exact.logical);
    let alloc = |f: &str| {
        size_of_file(&root.join(f), &exact_policy)
            .links
            .expect("links_out")
            .alloc_first_seen
    };
    let (big, small) = (alloc("big.bin"), alloc("small.bin"));
    assert!(big > 0 && small > 0, "big={big} small={small}");
    // Exact: each file's allocation once, and nothing single-linked.
    assert_eq!(exact.released_floor, 0);
    assert_eq!(exact_walk.links().expect("links_out").linked_inodes, 2);
    // Sizing: the big file once, the small one whole at each of its two names.
    assert_eq!(quick.released_floor, 2 * small);
    assert_eq!(
        quick.alloc_fair - exact.alloc_fair,
        2 * small - small / 2 * 2
    );
    assert!(exact.alloc_fair >= big / 2 * 2 + small / 2 * 2);
    assert!(
        exact.alloc_fair < big + big / 2,
        "the big file counted twice"
    );
}

#[test]
fn a_junction_is_a_link_to_a_look_and_a_refusal_to_a_walk() {
    let (_tmp, root, outside) = scratch();
    let policy = policy();
    let e = root.join("e");
    assert_eq!(marker_at(&e, "to-outside", &policy), Ok(Some(Marker::Link)));
    assert_eq!(marker_at(&e, "tool", &policy), Ok(Some(Marker::File)));
    assert_eq!(marker_at(&root, "e", &policy), Ok(Some(Marker::Dir)));
    assert_eq!(marker_at(&root, "nothing", &policy), Ok(None));
    assert_eq!(marker_at(&root, r"e\tool", &policy), Err(Refusal::Relative));
    assert_eq!(
        link_target(&e, "to-outside", &policy),
        Ok(Some(canonical(&outside.join("big")).expect("absolute")))
    );
    assert_eq!(link_target(&e, "tool", &policy), Ok(None));
    assert_eq!(link_target(&e, "nothing", &policy), Ok(None));
    for through in [e.join("to-outside"), e.join("to-outside").join("sub")] {
        assert_eq!(
            Walk::begin(&RootSpec::new(&through), &policy).err(),
            Some(Refusal::Link),
            "{}",
            through.display()
        );
        assert_eq!(
            entry_count_of(&through, &policy),
            Err(Refusal::Link),
            "{}",
            through.display()
        );
    }
    assert_eq!(
        Walk::begin(&RootSpec::new(e.join("tool")), &policy).err(),
        Some(Refusal::NotDirectory)
    );
    assert_eq!(
        Walk::begin(&RootSpec::new(root.join("nothing")), &policy).err(),
        Some(Refusal::Gone)
    );
}

#[test]
fn a_folder_that_denies_its_listing_is_a_named_refusal() {
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let root = tmp.path().join("root");
    std::fs::create_dir_all(root.join("open")).expect("mkdir");
    write(&root.join("open/seen.bin"), 2_000);
    let _locked = Locked::new(&root.join("locked"));
    let policy = policy();
    let walk = walk_all(&root, &policy);
    let got = walk.tree().root().subtree;
    assert_eq!((got.files, got.dirs, got.logical), (1, 2, 2_000));
    assert_eq!(got.unreadable, 1);
    let locked = walk.tree().find(Path::new("locked")).expect("recorded");
    let rec = walk.tree().get(locked).expect("record");
    assert_eq!(rec.refused, Some(Refusal::Unreadable(ERROR_ACCESS_DENIED)));
    assert!(rec.complete);
    assert_eq!(
        walk.tree().refused(),
        vec![(locked, Refusal::Unreadable(ERROR_ACCESS_DENIED))]
    );
    assert_eq!(
        Walk::begin(&RootSpec::new(root.join("locked")), &policy).err(),
        Some(Refusal::Unreadable(ERROR_ACCESS_DENIED))
    );
}

#[test]
fn slices_end_at_the_budget_resume_to_the_same_tree_and_a_cancel_closes() {
    let (_tmp, root, _outside) = scratch();
    let mut policy = policy();
    let whole = settled_walk(&root, &policy);

    // Every clock read is a whole budget later: each slice is one listing call or one open.
    policy.slice = Duration::from_millis(1);
    let mut walker = Walker::new();
    let ticket = walker.start(&RootSpec::new(&root), &policy).expect("start");
    let mut clock = tick_clock(Duration::from_millis(1));
    let mut slices = 0u32;
    loop {
        match walker.slice(ticket, &mut clock) {
            Sliced::Step(Step::More { .. }) => slices += 1,
            Sliced::Step(Step::Done) => break,
            other => panic!("{other:?}"),
        }
    }
    assert!(slices > 20, "{slices} slices");
    assert!(!walker.armed());
    let sliced = walker.walk().expect("the walk");
    assert_eq!(sliced.tree().root().subtree, whole.tree().root().subtree);
    assert_eq!(sliced.walked(), whole.walked());
    assert_eq!(sliced.calls(), whole.calls());

    let second = walker.start(&RootSpec::new(&root), &policy).expect("start");
    assert_eq!(walker.slice(ticket, &mut clock), Sliced::Stale);
    assert!(matches!(
        walker.slice(second, &mut clock),
        Sliced::Step(Step::More { .. })
    ));
    walker.cancel();
    assert!(!walker.armed());
    assert_eq!(
        walker.slice(second, &mut clock),
        Sliced::Step(Step::Cancelled)
    );
    let partial: Sums = walker.walk().expect("the walk").partial();
    assert!(partial.files <= FILES);
}

#[test]
fn a_path_past_max_path_is_walked_and_is_a_root() {
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let root = tmp.path().join("root");
    let mut deep = root.clone();
    for i in 0..16 {
        deep.push(format!("level-{i:02}-with-a-name-thirty-two-long"));
    }
    assert!(deep.as_os_str().len() > 500, "{}", deep.as_os_str().len());
    std::fs::create_dir_all(&deep).expect("mkdir deep");
    write(&deep.join("bottom.bin"), 1_234);
    let policy = policy();
    let got = walk_all(&root, &policy).tree().root().subtree;
    assert_eq!((got.files, got.dirs, got.logical), (1, 16, 1_234));
    let bottom = walk_all(&deep, &policy).tree().root().subtree;
    assert_eq!((bottom.files, bottom.dirs, bottom.logical), (1, 0, 1_234));
    assert_eq!(entry_count_of(&deep, &policy), Ok(1));
    assert_eq!(
        marker_at(&deep, "bottom.bin", &policy),
        Ok(Some(Marker::File))
    );
}

#[test]
fn a_file_root_a_level_and_a_count_read_what_is_there() {
    let (_tmp, root, _outside) = scratch();
    let policy = policy();
    let file = size_of_file(&root.join("a/b/two.bin"), &policy);
    assert!(file.complete);
    assert_eq!(file.walked, 1);
    assert_eq!((file.sums.files, file.sums.logical), (1, 70_000));
    assert_eq!(file.links, None);
    assert_eq!(
        diskwalk_size_full(
            &RootSpec::new(root.join("e/to-outside")),
            &policy,
            Duration::from_secs(60),
            &mut tick_clock(Duration::from_micros(10)),
        )
        .err(),
        Some(Refusal::Link)
    );
    let tree = diskwalk_size_full(
        &RootSpec::new(&root),
        &policy,
        Duration::from_secs(60),
        &mut tick_clock(Duration::from_micros(10)),
    )
    .expect("a tree root");
    assert!(tree.complete);
    assert_eq!((tree.sums.files, tree.sums.logical), (FILES, LOGICAL));

    assert_eq!(entry_count_of(&root.join("e"), &policy), Ok(4));
    assert_eq!(entry_count_of(&root.join("empty"), &policy), Ok(0));

    let rows = list_one_level(&RootSpec::new(root.join("e")), &policy).expect("one level");
    let seen: Vec<(String, bool)> = rows
        .iter()
        .map(|r| (r.name.to_string_lossy().into_owned(), r.dir))
        .collect();
    assert_eq!(
        seen,
        vec![
            ("four-again.bin".to_owned(), false),
            ("shared.bin".to_owned(), false),
            ("to-outside".to_owned(), false),
            ("tool".to_owned(), false),
        ]
    );
    assert!(
        rows.iter()
            .all(|r| r.refused.is_none() && !r.mount_boundary)
    );
    assert!(rows[0].alloc > 0 && rows[1].alloc > 0 && rows[3].alloc > 0);
    assert_eq!(rows[2].alloc, 0);
    let top = list_one_level(&RootSpec::new(&root), &policy).expect("one level");
    assert_eq!(top.iter().filter(|r| r.dir).count(), 4);
}

#[test]
fn looks_find_a_worktree_and_a_build_folder_above() {
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(repo.join(".git")).expect("mkdir");
    std::fs::create_dir_all(repo.join("src/deep")).expect("mkdir");
    std::fs::create_dir_all(repo.join("target/debug/deps")).expect("mkdir");
    write(&repo.join("target/CACHEDIR.TAG"), 10);
    write(&repo.join("target/debug/.cargo-lock"), 0);
    let policy = policy();
    let canon = canonical(&repo).expect("absolute");
    assert_eq!(
        worktree_above(&repo.join("src/deep"), &policy),
        Some(Worktree {
            root: canon.clone(),
            linked: false
        })
    );
    assert_eq!(
        build_dir_above(&repo.join("target/debug/deps"), &policy),
        Some(canon.join("target"))
    );
    let (dev, ino) = directory_identity(&repo, &policy).expect("identity");
    assert_eq!(
        directory_identity(&repo.join("src").join("..").join("src"), &policy),
        Err(Refusal::Relative)
    );
    let again = directory_identity(Path::new(&format!(r"\\?\{}", canon.display())), &policy);
    assert_eq!(again, Ok((dev, ino)));
    assert_ne!(
        directory_identity(&repo.join("src"), &policy),
        Ok((dev, ino))
    );
}

#[test]
fn a_cloud_placeholder_is_named_and_never_entered() {
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let root = tmp.path().join("root");
    std::fs::create_dir_all(root.join("cloud/inner")).expect("mkdir");
    write(&root.join("cloud/inner/remote.bin"), 9_000);
    write(&root.join("local.bin"), 3_000);
    let policy = policy();
    let (_, ino) = directory_identity(&root.join("cloud"), &policy).expect("identity");
    crate::win::force_dataless_ino_for_test(Some(ino));
    let (walk, seen) = crate::touched::record(|| walk_all(&root, &policy));
    let begin = Walk::begin(&RootSpec::new(root.join("cloud")), &policy).err();
    let inside = marker_at(&root.join("cloud/inner"), "remote.bin", &policy);
    crate::win::force_dataless_ino_for_test(None);
    let got = walk.tree().root().subtree;
    assert_eq!((got.files, got.dirs, got.logical), (1, 1, 3_000));
    let cloud = walk.tree().find(Path::new("cloud")).expect("recorded");
    assert_eq!(
        walk.tree().get(cloud).expect("record").refused,
        Some(Refusal::Dataless)
    );
    assert_eq!(seen, vec![canonical(&root).expect("absolute")]);
    assert_eq!(begin, Some(Refusal::Dataless));
    assert_eq!(inside, Err(Refusal::Dataless));
}

#[test]
fn without_the_by_name_read_a_walk_still_lists_and_a_look_is_unsupported() {
    let (_tmp, root, _outside) = scratch();
    let mut policy = policy();
    policy.links_out = true;
    let with = walk_all(&root, &policy).tree().root().subtree;
    crate::win::force_no_query_by_name_for_test(true);
    let without = walk_all(&root, &policy).tree().root().subtree;
    let look = marker_at(&root, "e", &policy);
    let file = diskwalk_size_full(
        &RootSpec::new(root.join("top.bin")),
        &policy,
        Duration::from_secs(60),
        &mut tick_clock(Duration::from_micros(10)),
    )
    .err();
    crate::win::force_no_query_by_name_for_test(false);
    assert_eq!(
        (without.files, without.dirs, without.logical),
        (with.files, with.dirs, with.logical)
    );
    // Every file reads as one link: nothing is shared out of its folder's sum.
    assert_eq!(without.released_floor, without.alloc_fair);
    assert!(without.alloc_fair > with.alloc_fair);
    assert_eq!(look, Err(Refusal::Unsupported));
    assert_eq!(file, Some(Refusal::Unsupported));
}

/// A walk of `root` whose every slice is one unit: each clock read is a whole budget later.
fn unit_walk(root: &Path, policy: &Policy) -> (Walk, impl FnMut() -> Instant + use<>) {
    let mut policy = policy.clone();
    policy.slice = Duration::from_millis(1);
    let walk = Walk::begin(&RootSpec::new(root), &policy).expect("begin");
    (walk, tick_clock(Duration::from_millis(1)))
}

/// Slice until `there`; the walk must not end first.
fn slice_until(walk: &mut Walk, clock: &mut dyn FnMut() -> Instant, there: impl Fn(&Walk) -> bool) {
    let mut slices = 0u32;
    while !there(walk) {
        assert!(
            matches!(walk.diskwalk_slice(clock), Step::More { .. }),
            "the walk ended first"
        );
        slices += 1;
        assert!(slices < 100_000, "the walk is not getting anywhere");
    }
}

fn finish(walk: &mut Walk, clock: &mut dyn FnMut() -> Instant) {
    let mut slices = 0u32;
    while let Step::More { .. } = walk.diskwalk_slice(clock) {
        slices += 1;
        assert!(slices < 100_000, "the walk is not getting anywhere");
    }
    assert!(walk.is_done());
}

/// The record of the folder `rel` under the walk's root, once the walk has listed its name.
fn rec<'a>(walk: &'a Walk, rel: &str) -> Option<&'a crate::DirRec> {
    walk.tree()
        .find(Path::new(rel))
        .and_then(|id| walk.tree().get(id))
}

fn listed(walk: &Walk, rel: &str) -> u32 {
    rec(walk, rel).map_or(0, |r| r.direct_entries)
}

/// A rename's outcome as its Win32 code.
fn rename(from: &Path, to: &Path) -> Result<(), Option<i32>> {
    std::fs::rename(from, to).map_err(|e| e.raw_os_error())
}

fn many_files(dir: &Path, n: usize) {
    std::fs::create_dir_all(dir).expect("mkdir");
    for i in 0..n {
        write(&dir.join(format!("f{i:04}.bin")), 1 + i % 3);
    }
}

#[test]
fn a_file_created_and_still_open_is_sized_as_it_is_now() {
    use std::io::Write as _;
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let root = tmp.path().join("root");
    std::fs::create_dir_all(&root).expect("mkdir");
    write(&root.join("closed.bin"), 5_000);
    let mut held = std::fs::File::create(root.join("held.bin")).expect("create");
    held.write_all(&vec![7u8; 300_000]).expect("write");
    let policy = policy();
    // NEGATIVE CONTROL: the folder's index lists the open file as empty, so a walk that took
    // the index's word would count nothing for it.
    let handle = crate::walk::imp::open_checked(&canonical(&root).expect("absolute"), &policy)
        .expect("open");
    let mut index = Vec::new();
    let mut lister = Lister::new(Vec::new(), Vec::new(), LinkReads::None);
    while lister
        .next_batch(&handle, &mut |e| {
            index.push((String::from_utf16_lossy(e.name), e.logical));
        })
        .expect("list")
    {}
    index.sort();
    assert_eq!(
        index,
        vec![("closed.bin".to_owned(), 5_000), ("held.bin".to_owned(), 0)]
    );
    // A sizing walk (well under the 1 MiB floor) reads it by name, as it is now.
    let got = walk_all(&root, &policy).tree().root().subtree;
    assert_eq!((got.files, got.logical), (2, 305_000));
    assert!(got.alloc_fair >= 300_000, "{}", got.alloc_fair);
    drop(held);
}

#[test]
fn an_exact_walk_reads_every_file_as_it_is_now() {
    use std::io::Write as _;
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let root = tmp.path().join("root");
    std::fs::create_dir_all(&root).expect("mkdir");
    let floor = usize::try_from(crate::walk::imp::LINK_READ_FLOOR).expect("fits");
    let grown = u64::try_from(1_000 + 2 * floor).expect("fits");
    // A log last closed small, reopened and still open while it grows.
    write(&root.join("log.txt"), 1_000);
    let mut log = std::fs::OpenOptions::new()
        .append(true)
        .open(root.join("log.txt"))
        .expect("reopen");
    log.write_all(&vec![7u8; 2 * floor]).expect("append");
    // A hard link made while the file was small, which then grew through its first name.
    write(&root.join("a.bin"), 1_000);
    std::fs::hard_link(root.join("a.bin"), root.join("b.bin")).expect("link");
    std::fs::OpenOptions::new()
        .append(true)
        .open(root.join("a.bin"))
        .expect("reopen")
        .write_all(&vec![7u8; 2 * floor])
        .expect("append");
    let quick = walk_all(&root, &policy()).tree().root().subtree;
    let mut exact_policy = policy();
    exact_policy.links_out = true;
    let exact_walk = walk_all(&root, &exact_policy);
    let exact = exact_walk.tree().root().subtree;
    drop(log);
    // Exact: every name at its file's size now, and the linked file's allocation once.
    assert_eq!((exact.files, exact.logical), (3, 3 * grown));
    let links = exact_walk.links().expect("links_out");
    assert_eq!(links.linked_inodes, 1);
    assert_eq!(links.shared_out, 0);
    assert!(links.alloc_first_seen >= 2 * grown, "{links:?}");
    assert!(links.alloc_first_seen < 3 * grown, "the link counted twice");
    // Sizing: the same names, and a size the index holds is one the file had: never more.
    assert_eq!(quick.files, 3);
    assert!(quick.logical <= exact.logical, "{quick:?}");
    assert!(quick.alloc_fair <= exact.alloc_fair, "{quick:?}");
}

#[test]
fn a_walk_released_after_every_slice_reads_the_same_tree() {
    let (_tmp, root, _outside) = scratch();
    many_files(&root.join("many"), 700);
    for exact in [false, true] {
        let mut policy = policy();
        policy.links_out = exact;
        let whole = settled_walk(&root, &policy);
        let (mut walk, mut clock) = unit_walk(&root, &policy);
        let mut slices = 0u32;
        while let Step::More { .. } = walk.diskwalk_slice(&mut clock) {
            walk.release();
            slices += 1;
            // A released listing that started over would never end: fail, not hang.
            assert!(slices < 100_000, "the walk is not getting anywhere");
        }
        assert!(walk.is_done());
        assert!(slices > 20, "{slices} slices");
        assert_eq!(walk.tree(), whole.tree(), "exact={exact}");
        assert_eq!(
            (walk.walked(), walk.calls(), walk.links()),
            (whole.walked(), whole.calls(), whole.links()),
            "exact={exact}"
        );
    }
}

#[test]
fn a_released_listing_goes_on_after_the_last_name_it_was_given() {
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let dir = tmp.path().join("dir");
    many_files(&dir, 400);
    let policy = policy();
    let canon = canonical(&dir).expect("absolute");
    let open = || crate::walk::imp::open_checked(&canon, &policy).expect("open");
    let all: Vec<String> = (0..400).map(|i| format!("f{i:04}.bin")).collect();
    let batch = |lister: &mut Lister, handle: &std::os::windows::io::OwnedHandle| {
        let mut names = Vec::new();
        let more = lister
            .next_batch(handle, &mut |e| {
                names.push(String::from_utf16_lossy(e.name));
            })
            .expect("list");
        (names, more)
    };
    let rest = |lister: &mut Lister, handle: &std::os::windows::io::OwnedHandle| {
        let mut names = Vec::new();
        loop {
            let (got, more) = batch(lister, handle);
            names.extend(got);
            if !more {
                return names;
            }
        }
    };
    let new = || Lister::new(Vec::new(), Vec::new(), LinkReads::None);

    // Released after its first buffer: the next handle goes on after that buffer's last name.
    let mut lister = new();
    let first = open();
    let (mut names, more) = batch(&mut lister, &first);
    assert!(
        more && names.len() > 50 && names.len() < 300,
        "{}",
        names.len()
    );
    assert!(lister.release());
    drop(first);
    let last: Vec<u16> = names.last().expect("some").encode_utf16().collect();
    assert_eq!(lister.resume_name(), Some(last.as_slice()));
    names.extend(rest(&mut lister, &open()));
    assert_eq!(names, all);

    // The name it was to go on after is deleted meanwhile: every other name still comes once.
    let mut lister = new();
    let first = open();
    let (mut names, _) = batch(&mut lister, &first);
    assert!(lister.release());
    drop(first);
    std::fs::remove_file(dir.join(names.last().expect("some"))).expect("delete");
    names.extend(rest(&mut lister, &open()));
    assert_eq!(names, all);

    // Too far in to find its place again: nothing is remembered, and the handle it keeps
    // lists on.
    crate::win::force_skip_max_for_test(Some(50));
    let mut lister = new();
    let first = open();
    let (mut names, _) = batch(&mut lister, &first);
    let kept = !lister.release();
    crate::win::force_skip_max_for_test(None);
    assert!(kept && lister.resume_name().is_none());
    names.extend(rest(&mut lister, &first));
    assert_eq!(names.len(), 399);
}

#[test]
fn a_released_walk_lets_the_folders_above_its_cursor_be_renamed() {
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let outer = tmp.path().join("outer");
    let proj = outer.join("proj");
    many_files(&proj.join("a/b/c/d"), 400);
    many_files(&proj.join("a/side"), 3);
    write(&proj.join("a/b/in-b.bin"), 9_000);
    let policy = policy();
    let whole = settled_walk(&proj, &policy);
    let (mut walk, mut clock) = unit_walk(&proj, &policy);
    // Part way through the listing of `d`, four levels down.
    slice_until(&mut walk, &mut clock, |w| listed(w, "a/b/c/d") > 0);
    let at = listed(&walk, "a/b/c/d");
    assert!(at < 400, "{at}");
    let (outer2, a, a2) = (tmp.path().join("outer2"), proj.join("a"), proj.join("a2"));
    // NEGATIVE CONTROL: with the cursor's directories open, Windows refuses to rename any
    // folder above it (access denied), inside the root or above the root.
    assert_eq!(rename(&outer, &outer2), Err(Some(ERROR_ACCESS_DENIED)));
    assert_eq!(rename(&a, &a2), Err(Some(ERROR_ACCESS_DENIED)));
    walk.release();
    assert_eq!(rename(&outer, &outer2), Ok(()));
    assert_eq!(rename(&outer2, &outer), Ok(()));
    assert_eq!(rename(&a, &a2), Ok(()));
    assert_eq!(rename(&a2, &a), Ok(()));
    assert_eq!(rename(&proj.join("a/side"), &proj.join("a/side2")), Ok(()));
    assert_eq!(rename(&proj.join("a/side2"), &proj.join("a/side")), Ok(()));
    // The walk goes on where it was, and reads the tree an undisturbed walk reads.
    finish(&mut walk, &mut clock);
    assert_eq!(walk.tree(), whole.tree());
    assert_eq!(walk.walked(), whole.walked());
}

#[test]
fn a_directory_too_far_into_its_listing_stays_open_until_it_ends() {
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let outer = tmp.path().join("outer");
    let proj = outer.join("proj");
    many_files(&proj.join("d"), 400);
    let policy = policy();
    let whole = settled_walk(&proj, &policy);
    let outer2 = tmp.path().join("outer2");
    crate::win::force_skip_max_for_test(Some(50));
    let (mut walk, mut clock) = unit_walk(&proj, &policy);
    slice_until(&mut walk, &mut clock, |w| listed(w, "d") > 50);
    let part = listed(&walk, "d");
    walk.release();
    let held = rename(&outer, &outer2);
    slice_until(&mut walk, &mut clock, |w| listed(w, "d") == 400);
    // One more unit finds the listing's end.
    let more = walk.diskwalk_slice(&mut clock);
    walk.release();
    let free = rename(&outer, &outer2).and_then(|()| rename(&outer2, &outer));
    crate::win::force_skip_max_for_test(None);
    assert!(part < 400, "{part}");
    assert!(matches!(more, Step::More { .. }), "{more:?}");
    assert_eq!(held, Err(Some(ERROR_ACCESS_DENIED)));
    assert_eq!(free, Ok(()));
    finish(&mut walk, &mut clock);
    assert_eq!(walk.tree(), whole.tree());
}

#[test]
fn a_directory_replaced_while_the_walk_is_released_is_gone_not_walked() {
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let root = tmp.path().join("root");
    many_files(&root.join("x/d/inner"), 2);
    many_files(&root.join("x/d"), 400);
    write(&root.join("keep.bin"), 5_000);
    let policy = policy();
    let (mut walk, mut clock) = unit_walk(&root, &policy);
    slice_until(&mut walk, &mut clock, |w| listed(w, "x/d") > 0);
    let before = listed(&walk, "x/d");
    assert!(before < 400, "{before}");
    walk.release();
    // Another directory takes the name: the one the walk was in is elsewhere now.
    assert_eq!(rename(&root.join("x/d"), &root.join("x/d-old")), Ok(()));
    std::fs::create_dir(root.join("x/d")).expect("mkdir");
    write(&root.join("x/d/new.bin"), 70_000);
    finish(&mut walk, &mut clock);
    let d = rec(&walk, "x/d").expect("recorded");
    assert_eq!(d.refused, Some(Refusal::Gone));
    assert!(d.complete);
    assert_eq!(d.direct_entries, before);
    let got = walk.tree().root().subtree;
    // What was counted in it stays; the newcomer's file and the old one's rest are not read.
    assert!(got.logical >= 5_000 && got.logical < 70_000, "{got:?}");
    assert!(got.gone >= 1, "{got:?}");
    assert!(walk.tree().dirs().iter().all(|r| r.complete));
    assert!(!names(&walk).iter().any(|n| n == "d-old"));

    // The root itself renamed away while released: the walk ends, with what it had counted.
    let (mut walk, mut clock) = unit_walk(&root.join("x/d-old"), &policy);
    slice_until(&mut walk, &mut clock, |w| w.walked() > 0);
    walk.release();
    assert_eq!(
        rename(&root.join("x/d-old"), &root.join("x/d-older")),
        Ok(())
    );
    finish(&mut walk, &mut clock);
    let top = walk.tree().root();
    assert_eq!(top.refused, Some(Refusal::Gone));
    assert!(top.subtree.files > 0 && top.subtree.files < 400, "{top:?}");
}

#[test]
fn a_volume_mounted_at_a_folder_is_entered_only_where_the_table_names_it() {
    use aterm_sysprobe::volumes::{Mount, mount_of};
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let tree = tmp.path().join("tree");
    std::fs::create_dir_all(tree.join("sub")).expect("mkdir");
    write(&tree.join("one.bin"), 3_000);
    write(&tree.join("sub/two.bin"), 4_000);
    let elsewhere = tmp.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).expect("mkdir");
    // `mnt` stands for a folder a volume is mounted at. It is a junction to an empty scratch
    // folder, read (on this thread) as a mount of this volume's root: the tests never make a
    // link to a whole volume.
    let mnt = tmp.path().join("mnt");
    make_junction(&mnt, &elsewhere);
    let plain = policy();
    let canon = canonical(&tree).expect("absolute");
    let volume = mount_of(&canon, &plain.mounts)
        .and_then(|m| m.container.clone())
        .expect("the scratch tree's volume");
    // The tree's path from its volume's root, under the mounted folder.
    let through = mnt.join(canon.components().skip(2).collect::<PathBuf>());
    let named = |container: &str| {
        let mut policy = policy();
        policy.mounts.mounts.push(Mount {
            on: canonical(&mnt).expect("absolute"),
            from: "test".to_owned(),
            fstype: String::new(),
            local: true,
            automounted: false,
            removable: false,
            read_only: false,
            container: Some(container.to_owned()),
        });
        policy
    };
    let begin = |policy: &Policy| Walk::begin(&RootSpec::new(&through), policy).err();
    let target = format!(
        r"\??\{}",
        canonical(&elsewhere).expect("absolute").display()
    );
    let direct = settled_walk(&tree, &plain);

    // NEGATIVE CONTROLS. A junction to a directory is a link, whatever the table says of it.
    assert_eq!(begin(&plain), Some(Refusal::Link));
    assert_eq!(begin(&named(&volume)), Some(Refusal::Link));
    crate::win::force_volume_mount_for_test(Some((target, volume.clone())));
    // A mounted volume the table does not name there, or names as another volume, is a link.
    let unnamed = begin(&plain);
    let other = begin(&named("Volume{00000000-0000-0000-0000-000000000000}"));
    // Named by the table as the volume its reparse data names: entered at the volume's root.
    let vouched = named(&volume);
    let hopped = Walk::begin(&RootSpec::new(&through), &vouched).map(|_| ());
    let walked = hopped.is_ok().then(|| walk_all(&through, &vouched));
    let identity = directory_identity(&through, &vouched);
    let count = entry_count_of(&through, &vouched);
    crate::win::force_volume_mount_for_test(None);
    assert_eq!(unnamed, Some(Refusal::Link));
    assert_eq!(other, Some(Refusal::Link));
    assert_eq!(hopped, Ok(()));
    let walked = walked.expect("walked");
    assert_eq!(walked.tree().root().subtree, direct.tree().root().subtree);
    assert_eq!(walked.root_dev(), direct.root_dev());
    assert_eq!(identity, directory_identity(&tree, &plain));
    assert_eq!(count, Ok(2));
}

#[test]
fn only_a_volume_root_reads_as_a_mounted_volume() {
    let name = crate::win::volume_root_name_for_test;
    let volume = "Volume{6140fc3b-044f-43d7-b060-cb98170c7a63}";
    assert_eq!(name(&format!(r"\??\{volume}\")), Some(volume));
    for not in [
        format!(r"\??\{volume}"),
        format!(r"\??\{volume}\Users"),
        format!(r"\\?\{volume}\"),
        r"\??\C:\Users".to_owned(),
        r"\??\C:\".to_owned(),
        r"\??\Volume{}\".to_owned(),
        r"\??\Volume{6140fc3b-044f-43d7-b060-cb98170c7a63\..\x}\".to_owned(),
        String::new(),
    ] {
        assert_eq!(name(&not), None, "{not}");
    }
}

#[test]
fn a_configured_root_is_denied_whatever_a_mac_protects_by_default() {
    let home = Path::new(HOME);
    let named = ["~/Documents".to_owned(), "~/Secrets".to_owned()];
    let deny = Deny::for_home(home, &named, &[]);
    for sub in ["Documents", "documents", "Secrets"] {
        assert_eq!(
            deny.denies(&home.join(sub).join("x")),
            Some(Refusal::Protected),
            "{sub}"
        );
    }
    // Only what is named: no other folder of a Windows home is denied.
    for sub in ["Desktop", "Downloads", "Library", "src"] {
        assert_eq!(deny.denies(&home.join(sub).join("x")), None, "{sub}");
    }
    assert_eq!(deny.roots().len(), 2);
    // NEGATIVE CONTROL: nothing named, nothing denied.
    assert!(Deny::for_home(home, &[], &[]).roots().is_empty());
    let policy = Policy::new(deny, aterm_sysprobe::volumes::table());
    assert_eq!(
        Walk::begin(&RootSpec::new(home.join("Documents").join("x")), &policy).err(),
        Some(Refusal::Protected)
    );
}

#[test]
fn no_private_size_is_claimed_where_files_can_share_blocks() {
    let claims = crate::walk::imp::claims_private;
    assert!(claims(true, false));
    assert!(!claims(true, true) && !claims(false, false) && !claims(false, true));
    let (_tmp, root, _outside) = scratch();
    let mut policy = policy();
    policy.private = true;
    policy.links_out = true;
    let file = root.join("top.bin");
    crate::win::force_volume_flags_for_test(Some(0));
    let plain_walk = walk_all(&root, &policy);
    let plain_file = size_of_file(&file, &policy);
    crate::win::force_volume_flags_for_test(Some(crate::win::FILE_SUPPORTS_BLOCK_REFCOUNTING));
    let cloning_walk = walk_all(&root, &policy);
    let cloning_file = size_of_file(&file, &policy);
    crate::win::force_volume_flags_for_test(None);
    let (plain, cloning) = (
        plain_walk.tree().root().subtree,
        cloning_walk.tree().root().subtree,
    );
    // A volume that shares no blocks: a file's private size is its share of its allocation.
    assert!(
        plain
            .private
            .is_some_and(|p| p > 0 && p <= plain.alloc_fair),
        "{plain:?}"
    );
    assert_eq!(plain_file.sums.private, Some(plain_file.sums.alloc_fair));
    // One that can: no private size, and every other figure as before.
    assert_eq!(
        cloning,
        Sums {
            private: None,
            ..plain
        }
    );
    assert_eq!(cloning_walk.links(), plain_walk.links());
    assert_eq!(
        cloning_file.sums,
        Sums {
            private: None,
            ..plain_file.sums
        }
    );
}
