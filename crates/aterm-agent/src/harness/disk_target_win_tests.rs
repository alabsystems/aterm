// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Windows tests for the build-directory reclaim's Windows half: what NTFS adds to the unix
//! story — a lock another handle holds with `LockFileEx`, a process working in the directory,
//! junctions and a folder a volume is mounted on (one tag), a third party's reparse point,
//! uplifts that are NTFS hard links of an unhashed twin, owners read through each entry's
//! handle, drives that are not fixed disks, one path in many spellings, and paths past
//! `MAX_PATH`.
//!
//! Every fixture is laid out as cargo lays a build directory ([`lay_target`]) under its own
//! scratch directory; the only process started is a test's own `cmd.exe`, which ends when its
//! stdin is closed.

use super::*;
use crate::harness::disk::discover;
use std::os::windows::fs::OpenOptionsExt as _;

/// One of cargo's locks held as cargo holds it: opened by a handle of its own (std's
/// read-write-delete sharing) and locked whole with `File::lock` (`LockFileEx`, exclusive),
/// released by `unlock` on drop.
struct CargoHolds(std::fs::File);

impl CargoHolds {
    fn lock(path: &Path) -> CargoHolds {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .expect("the lock exists");
        #[expect(
            clippy::disallowed_methods,
            reason = "a test's build: the guard's Drop releases by unlock"
        )]
        let taken = file.lock();
        taken.expect("uncontended");
        CargoHolds(file)
    }
}

impl Drop for CargoHolds {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// A `cmd.exe` of this test's whose working directory is `dir`, blocked reading its stdin
/// until dropped: the drop closes the stdin, so it reads end-of-file and exits, and waits for
/// that exit.
struct Worker(aterm_uds::spawnfd::Child);

impl Worker {
    fn start(dir: &Path) -> Worker {
        let child = aterm_uds::spawnfd::command("cmd")
            .args(["/D", "/Q", "/C", "set /p line="])
            .current_dir(plain(dir))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("cmd starts");
        Worker(child)
    }

    fn id(&self) -> u32 {
        self.0.id()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.0.wait();
    }
}

/// A test's process universe: the processes `pids` (its own) and no other, each read for real.
fn among(
    pids: Vec<u32>,
) -> impl Fn(&[PathBuf], &[u32]) -> std::io::Result<aterm_uds::holders::PathScan> {
    move |dirs, skip| aterm_uds::holders::scan_path_users_of(&pids, dirs, skip)
}

/// What deleting everything under `root` releases, read independently of the reclaim: each
/// distinct file's allocation once, however many names it has inside.
fn allocated_once(root: &Path) -> u64 {
    let mut seen = std::collections::HashSet::new();
    let mut total = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).expect("read").flatten() {
            let meta = std::fs::symlink_metadata(e.path()).expect("stat");
            if meta.is_dir() {
                stack.push(e.path());
            } else if meta.is_file() {
                let id = aterm_sysprobe::volumes::file_id(&e.path(), false).expect("id");
                if seen.insert(id.ino) {
                    total += blocks(&e.path());
                }
            }
        }
    }
    total
}

/// A non-Microsoft, non-surrogate reparse tag: a third party's placeholder (a cloud file),
/// which a standard user may set ([`aterm_dirfd::seam::set_reparse_tag`]).
const FOREIGN_TAG: u32 = 0x0000_7A7E;

/// Make the file at `path` a third party's reparse point ([`FOREIGN_TAG`], no payload).
fn tag_foreign(path: &Path) {
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .expect("opened for the tag");
    aterm_dirfd::seam::set_reparse_tag(&file, FOREIGN_TAG, [0x7a; 16])
        .expect("FSCTL_SET_REPARSE_POINT");
}

/// A volume GUID name no volume carries: a stand-in mount point's target, never reachable.
const NO_VOLUME: &str = r"\??\Volume{00000000-0000-0000-0000-000000000000}\";

/// Make `path` (a new, empty folder) a folder a volume is mounted on, as
/// `SetVolumeMountPointW` writes one: the junction's tag onto [`NO_VOLUME`]. Set directly
/// ([`aterm_dirfd::seam::set_mount_point`]), it needs no administrator and mounts nothing.
fn mount_point(path: &Path) {
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    std::fs::create_dir(path).expect("the folder");
    let dir = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .expect("opened for the reparse point");
    aterm_dirfd::seam::set_mount_point(&dir, NO_VOLUME).expect("FSCTL_SET_REPARSE_POINT");
}

/// `p`'s plain spelling with every letter upper-cased: another spelling of the same path,
/// which only the one path-identity rule places.
fn shouted(p: &Path) -> PathBuf {
    PathBuf::from(plain(p).to_uppercase())
}

/// A build holding `.cargo-lock` through a handle of its own (`LockFileEx`, as cargo takes
/// it) keeps its profile, in the report and in the reclaim, and nothing is touched; released,
/// the reclaim takes only `incremental/` and every other entry is byte-identical.
#[test]
fn a_lock_another_handle_holds_keeps_the_profile_and_once_released_only_incremental_goes() {
    let tmp = Tmp::new("win-held");
    let t = synthetic_target(&tmp, "target");
    let debug = t.join("debug");
    let now = now_after_write(STALE + 1);
    let before = snapshot(&t, &|_| false);

    let hold = CargoHolds::lock(&debug.join(".cargo-lock"));
    let td = assessed(&t, now);
    let profile = td.profiles.first().expect("the debug profile");
    assert!(
        profile
            .in_use
            .as_deref()
            .is_some_and(|w| w.contains("a build holds") && w.contains(".cargo-lock")),
        "{td:?}"
    );
    target_witness(&td, now, STALE).expect_err("no idle profile");
    let err = reclaim(&t, &judge(now)).expect_err("a build holds it");
    assert!(err.to_string().contains("a build holds"), "{err}");
    drop(hold);
    assert_eq!(snapshot(&t, &|_| false), before, "nothing touched");

    let done = reclaim(&t, &judge(now)).expect("released: reclaimed");
    assert_eq!(done.units, vec![at(&t, "debug/incremental")], "{done:?}");
    assert!(
        done.skipped.is_empty() && done.trouble.is_empty(),
        "{done:?}"
    );
    assert_eq!(
        snapshot(&t, &in_incremental),
        before
            .into_iter()
            .filter(|(rel, _, _)| !in_incremental(rel))
            .collect::<Vec<_>>(),
        "every entry but incremental/ is byte-identical"
    );
    assert!(
        CARGO_LOCKS.iter().all(|l| !held(&debug.join(l))),
        "every lock released"
    );
}

/// A whole removal asks the caller, under every lock, whether a process works in the
/// directory: a `cmd.exe` whose working directory is inside refuses it, and nothing goes.
/// Once it has exited, with no process using it, the directory goes whole, and the bytes
/// counted are each file's allocation once — the uplifted `debug\lay.pdb` is an NTFS hard
/// link of its unhashed `deps\` twin, as cargo uplifts an MSVC binary's, and counts once.
#[test]
fn a_process_working_inside_keeps_a_whole_removal_and_once_it_exits_the_directory_goes() {
    let tmp = Tmp::new("win-whole-user");
    let t = synthetic_target(&tmp, "target");
    let debug = t.join("debug");
    std::fs::write(at(&debug, "deps/lay.pdb"), vec![3u8; 64 * 1024]).expect("pdb");
    std::fs::hard_link(at(&debug, "deps/lay.pdb"), debug.join("lay.pdb")).expect("uplift");
    let now = now_after_write(2);
    let before = snapshot(&t, &|_| false);

    let worker = Worker::start(&debug);
    let pid = worker.id();
    let scan = among(vec![pid]);
    let err = reclaim_whole_with(
        &t,
        &judge(now),
        now,
        DAY,
        &mut || discover::unused_under(std::slice::from_ref(&t), &scan),
        &mut |_, _| {},
    )
    .expect_err("a process works in it");
    let why = err.to_string();
    assert!(
        why.contains(&format!("process {pid}")) && why.contains("working directory"),
        "{why}"
    );
    drop(worker);
    assert_eq!(snapshot(&t, &|_| false), before, "nothing went");

    let expect = allocated_once(&t);
    assert!(expect >= 64 * 1024 + 8192 + 4096, "{expect}");
    assert_eq!(
        total_bytes(&t),
        (expect, false),
        "the size the report gives"
    );
    let nobody = among(Vec::new());
    let done = reclaim_whole_with(
        &t,
        &judge(now),
        now,
        DAY,
        &mut || discover::unused_under(std::slice::from_ref(&t), &nobody),
        &mut |_, _| {},
    )
    .expect("goes whole");
    assert_eq!(done.units, std::slice::from_ref(&t), "{done:?}");
    assert!(done.trouble.is_empty(), "{done:?}");
    assert_eq!(done.bytes, expect, "each file's allocation once");
    assert!(!t.exists());
}

/// A junction inside a build directory is removed as itself — by a whole removal and by an
/// idle reclaim of `incremental/` — and what it points at, outside, is byte-identical.
#[test]
fn a_junction_inside_is_removed_as_itself_and_what_it_points_at_survives() {
    let tmp = Tmp::new("win-junction");
    let outside = tmp.path().join("outside");
    lay_target(&outside);
    std::fs::write(outside.join("keep.txt"), vec![4u8; 16 * 1024]).expect("theirs");
    let outside_before = snapshot(&outside, &|_| false);

    let t = synthetic_target(&tmp, "whole");
    link_dir(&outside, &at(&t, "debug/deps/escape"));
    let now = now_after_write(2);
    let done = reclaim_whole_with(&t, &judge(now), now, DAY, &mut || Ok(()), &mut |_, _| {})
        .expect("goes whole");
    assert_eq!(done.units, std::slice::from_ref(&t), "{done:?}");
    assert!(!t.exists());
    assert_eq!(snapshot(&outside, &|_| false), outside_before);

    let t = synthetic_target(&tmp, "idle");
    link_dir(&outside, &at(&t, "debug/incremental/lay-1x2y3z/escape"));
    let done = reclaim(&t, &judge(now_after_write(STALE + 1))).expect("reclaimed");
    assert_eq!(done.units, vec![at(&t, "debug/incremental")], "{done:?}");
    assert_eq!(snapshot(&outside, &|_| false), outside_before);
}

/// A build directory deeper than `MAX_PATH` (260 UTF-16 units; long paths are off on this
/// host) is assessed, reclaimed and removed whole: every step past the volume root is relative
/// to a handle.
#[test]
fn a_build_directory_past_max_path_is_reclaimed_and_removed() {
    let tmp = Tmp::new("win-long");
    let mut deep = tmp.path().to_path_buf();
    for i in 0..6 {
        deep.push(format!("{i}-{}", "d".repeat(40)));
    }
    let t = std::fs::canonicalize(lay_target(&deep.join("target"))).expect("canonical");
    let cache = at(&t, "debug/incremental/lay-1x2y3z/s-abc-def/query-cache.bin");
    assert!(
        plain(&cache).encode_utf16().count() > 260,
        "{}",
        cache.display()
    );
    let now = now_after_write(STALE + 1);
    let expect = blocks(&cache);
    let td = assessed(&t, now);
    assert!(target_witness(&td, now, STALE).is_ok(), "{td:?}");
    assert_eq!(td.reclaimable(now, STALE), (expect, false));

    let done = reclaim(&t, &judge(now)).expect("reclaimed");
    assert_eq!(done.units, vec![at(&t, "debug/incremental")], "{done:?}");
    assert_eq!(done.bytes, expect, "{done:?}");
    let done = reclaim_whole_with(&t, &judge(now), now, DAY, &mut || Ok(()), &mut |_, _| {})
        .expect("goes whole");
    assert_eq!(done.units, std::slice::from_ref(&t), "{done:?}");
    assert!(!t.exists());
}

/// A third party's reparse point (a cloud placeholder) inside `incremental/` is never
/// entered or deleted: it is kept and said, its siblings still go, and `incremental/` is not
/// claimed as gone.
#[test]
fn a_third_partys_reparse_point_is_kept_and_said() {
    let tmp = Tmp::new("win-placeholder");
    let t = synthetic_target(&tmp, "target");
    let placeholder = at(&t, "debug/incremental/lay-1x2y3z/placeholder");
    std::fs::write(&placeholder, vec![6u8; 8192]).expect("placeholder");
    tag_foreign(&placeholder);

    let done = reclaim(&t, &judge(now_after_write(STALE + 1))).expect("partly reclaimed");
    assert!(
        std::fs::symlink_metadata(&placeholder).is_ok(),
        "the placeholder stays"
    );
    assert!(
        !at(&t, "debug/incremental/lay-1x2y3z/s-abc-def").exists(),
        "its siblings went"
    );
    assert!(done.units.is_empty(), "not claimed as gone: {done:?}");
    assert!(
        done.trouble.iter().any(|t| t.contains("placeholder")),
        "said: {done:?}"
    );
    assert!(done.bytes > 0, "{done:?}");
}

/// The owner of the directory at `dir`, read through a facts-only handle of its own.
fn owner_of(dir: &Path) -> aterm_dirfd::Sid {
    let pinned = aterm_dirfd::pin(dir).expect("pinned");
    let facts = aterm_dirfd::openat(
        &pinned,
        ".",
        aterm_dirfd::OFlags::PATH | aterm_dirfd::OFlags::DIRECTORY,
        aterm_dirfd::Mode::empty(),
    )
    .expect("a facts-only handle");
    aterm_dirfd::owner(&facts).expect("its owner")
}

/// Owners are read through each entry's own handle: a build directory of this user's
/// passes the other-user fence of a whole removal. Judged against another user (the seams
/// substitute identities, never verdicts: a standard user cannot give a file away), its
/// REAL owners fail it, said by name; and one entry deep inside whose owner reads as
/// `S-1-5-18`'s fails it alone — in the sizing walk and in the ownership walk alike — and
/// the whole removal's fences refuse it.
#[test]
fn another_users_entry_keeps_the_directory_from_going_whole() {
    let tmp = Tmp::new("win-owner");
    let t = synthetic_target(&tmp, "target");
    let ((_, partial), foreign) = total_bytes_owned(&t);
    assert_eq!(
        (partial, foreign),
        (false, None),
        "every entry is this user's"
    );
    assert_eq!(foreign_walk(&t, 1_000_000, FOREIGN_WALK_WALL), None);
    let cut = foreign_walk(&t, 3, FOREIGN_WALK_WALL).expect("cut short");
    assert!(cut.contains("more than 3 entries"), "{cut}");

    // Only SYSTEM passes: the owner this user's files really have is another user's.
    let owner = owner_of(&t);
    only_owners_pass(Some(vec![aterm_dirfd::Sid::local_system()]));
    let (_, foreign) = total_bytes_owned(&t);
    let walked = foreign_walk(&t, 1_000_000, FOREIGN_WALK_WALL);
    only_owners_pass(None);
    let foreign = foreign.expect("judged as another user's");
    assert!(foreign.contains(&format!("(owner {owner})")), "{foreign}");
    let walked = walked.expect("judged as another user's");
    assert!(walked.contains(&format!("(owner {owner})")), "{walked}");

    mark_theirs(&at(&t, "debug/deps/liblay.rlib"));
    let (_, foreign) = total_bytes_owned(&t);
    let foreign = foreign.expect("another user's entry");
    assert!(foreign.contains("(owner S-1-5-18)"), "{foreign}");
    let walked = foreign_walk(&t, 1_000_000, FOREIGN_WALK_WALL).expect("found");
    assert!(walked.contains("(owner S-1-5-18)"), "{walked}");

    let mut td = assessed(&t, now_after_write(2));
    td.foreign = Some(foreign);
    let table = aterm_sysprobe::volumes::table();
    let why = crate::harness::disk::whole_fences(&td, Some(&table)).expect_err("refused");
    assert!(why.contains("another user"), "{why}");
}

/// A folder a volume is mounted on, inside a build directory, is never removed — that would
/// unmount the volume — nor entered: it is kept and said as on another device, as unix keeps a
/// mount point, by an idle reclaim of `incremental/` (its siblings still go) and by a whole
/// removal (which then leaves the directory). A junction, the same tag onto a path, still goes
/// as itself ([`a_junction_inside_is_removed_as_itself_and_what_it_points_at_survives`]).
#[test]
fn a_volume_mounted_inside_is_kept_as_another_device() {
    let tmp = Tmp::new("win-mounted");
    let t = synthetic_target(&tmp, "idle");
    let mounted = at(&t, "debug/incremental/lay-1x2y3z/mounted");
    mount_point(&mounted);
    let counted = blocks(&at(
        &t,
        "debug/incremental/lay-1x2y3z/s-abc-def/query-cache.bin",
    ));
    let done = reclaim(&t, &judge(now_after_write(STALE + 1))).expect("partly reclaimed");
    assert!(
        std::fs::symlink_metadata(&mounted).is_ok(),
        "the mount point stays"
    );
    assert!(
        !at(&t, "debug/incremental/lay-1x2y3z/s-abc-def").exists(),
        "its siblings went"
    );
    assert!(done.units.is_empty(), "not claimed as gone: {done:?}");
    assert!(
        done.trouble
            .iter()
            .any(|w| w.contains("mounted") && w.contains("another device")),
        "said: {done:?}"
    );
    assert_eq!(done.bytes, counted, "{done:?}");

    let t = synthetic_target(&tmp, "whole");
    let mounted = at(&t, "debug/deps/mounted");
    mount_point(&mounted);
    let now = now_after_write(2);
    let done = reclaim_whole_with(&t, &judge(now), now, DAY, &mut || Ok(()), &mut |_, _| {})
        .expect("partly removed");
    assert!(std::fs::symlink_metadata(&mounted).is_ok(), "{done:?}");
    assert!(done.units.is_empty() && t.exists(), "{done:?}");
    assert!(
        done.trouble
            .iter()
            .any(|w| w.contains("mounted") && w.contains("another device")),
        "said: {done:?}"
    );
}

/// Tier 3 judges the entry its listing named, the one its delete is held to: a name that,
/// between the listing and the judgement, comes to name a true uplift (a hard link of its
/// `deps\` twin) is judged as the file listed — not cargo's: kept and said — and neither file
/// is deleted.
#[test]
fn tier_3_judges_the_file_it_listed_never_one_renamed_into_its_name() {
    let tmp = Tmp::new("win-judged");
    let t = synthetic_target(&tmp, "target");
    let debug = t.join("debug");
    let deps = debug.join("deps");
    std::fs::write(deps.join("lay.pdb"), vec![1u8; 16 * 1024]).expect("twin");
    std::fs::write(debug.join("lay.pdb"), vec![2u8; 16 * 1024]).expect("not its twin");
    let now = now_after_write(8);
    let mut swapped = false;
    let done = reclaim_profiles_with(
        &t,
        &judge(now),
        now,
        7 * DAY,
        &mut || Ok(()),
        &mut || Ok(()),
        &mut |stage, _| {
            // Listed, not yet judged.
            if stage == Stage::Deleting {
                std::fs::rename(debug.join("lay.pdb"), debug.join("lay.pdb.listed"))
                    .expect("the listed file moved aside");
                std::fs::hard_link(deps.join("lay.pdb"), debug.join("lay.pdb"))
                    .expect("a true uplift in its place");
                swapped = true;
            }
        },
    )
    .expect("emptied");
    assert!(swapped, "the hook ran");
    assert!(
        done.skipped
            .iter()
            .any(|(p, why)| p == &debug.join("lay.pdb") && why.contains("not one of cargo's")),
        "judged as the file listed: {done:?}"
    );
    assert!(
        !done.trouble.iter().any(|w| w.contains("lay.pdb")),
        "{done:?}"
    );
    assert_eq!(
        std::fs::read(debug.join("lay.pdb")).expect("the newcomer stays"),
        vec![1u8; 16 * 1024]
    );
    assert_eq!(
        std::fs::read(debug.join("lay.pdb.listed")).expect("the listed file stays"),
        vec![2u8; 16 * 1024]
    );
}

/// Only a fixed disk's build caches are reclaimed: a build directory whose volume reads as
/// not fixed (a removable stick) or of a kind that cannot be read (the drive seam: this
/// machine has fixed disks only) is no candidate — the report's fences say why — and every
/// reclaim, of each tier, refuses it before opening anything, everything byte-identical. Read
/// as fixed again, the same directory is reclaimed.
#[test]
fn a_build_directory_off_a_fixed_disk_is_never_a_candidate_nor_reclaimed() {
    let tmp = Tmp::new("win-drive");
    let t = synthetic_target(&tmp, "target");
    let debug = t.join("debug");
    let now = now_after_write(STALE + 1);
    let before = snapshot(&t, &|_| false);
    for (kind, why) in [(Some(false), "removable"), (None, "could not be read")] {
        pretend_drive_kind(Some(kind));
        let td = assessed(&t, now);
        let fence = td.fences().expect_err("off a fixed disk");
        assert!(
            fence.contains(why) && fence.contains("fixed disk"),
            "{fence}"
        );
        let witness = target_witness(&td, now, STALE).expect_err("no candidate");
        assert!(witness.contains(why), "{witness}");
        for (tier, refused) in [
            ("tier 2", reclaim(&t, &judge(now))),
            ("tier 4", reclaim_lru(&t, &debug, &judge(now), now)),
            (
                "tier 1",
                reclaim_whole_with(&t, &judge(now), now, DAY, &mut || Ok(()), &mut |_, _| {}),
            ),
            (
                "tier 3",
                reclaim_profiles(&t, &judge(now), now, DAY, &mut || Ok(()), &mut || Ok(())),
            ),
        ] {
            let err = refused.expect_err(tier).to_string();
            assert!(
                err.contains(why) && err.contains("fixed disk"),
                "{tier}: {err}"
            );
        }
        assert_eq!(snapshot(&t, &|_| false), before, "nothing touched");
    }
    pretend_drive_kind(None);
    assert!(assessed(&t, now).fences().is_ok());
    let done = reclaim(&t, &judge(now)).expect("a fixed disk's: reclaimed");
    assert_eq!(done.units, vec![at(&t, "debug/incremental")], "{done:?}");
}

/// The watch's device fence holds on Windows: it names the volume it measures by `dev_of` a
/// plain path there (its home; sysprobe's `FILE_ID_INFO` serial, links followed) and by the
/// GUID its free-space reading is keyed by (`super::volume_of`, as the host's look reads it),
/// and a reclaim reads both of its build directory's through the pinned handle (dirfd's): one
/// figure and one GUID, so a reclaim under that judge proceeds — tier 2 and tier 1 — where one
/// under any other refuses ([`the_delete_never_enters_another_device`],
/// [`a_clone_of_the_measured_volume_is_refused_by_its_guid`]).
#[test]
fn the_volume_the_watch_measures_is_the_one_the_reclaim_reads() {
    let tmp = Tmp::new("win-watch-device");
    let home = PathBuf::from(plain(tmp.path()));
    let device = dev_of(&home).expect("the measured volume");
    let table = aterm_sysprobe::volumes::table();
    let reading = crate::harness::disk::volume_of(&home, &table).expect("the watch's reading");
    let volume = VolumeGuid::of_key(&reading.key);
    assert!(
        volume.is_some(),
        "a Windows reading is keyed by its GUID: {:?}",
        reading.key
    );
    assert_eq!(
        VolumeGuid::of_path_now(&home),
        volume,
        "the search's reading"
    );
    let pinned = aterm_dirfd::pin(&home).expect("pinned");
    let through_handle = aterm_dirfd::volume_guid(&pinned).expect("the handle's volume");
    assert_eq!(
        VolumeGuid::parse(&through_handle),
        volume,
        "the reclaim's reading"
    );
    let watch = |now: i64| Judge {
        device: Some(device),
        volume,
        ..judge(now)
    };

    let t = synthetic_target(&tmp, "idle");
    let now = now_after_write(STALE + 1);
    assert_eq!(assessed(&t, now).device, Some(device));
    let done = reclaim(&t, &watch(now)).expect("the measured volume's: reclaimed");
    assert_eq!(done.units, vec![at(&t, "debug/incremental")], "{done:?}");

    let t = synthetic_target(&tmp, "whole");
    let now = now_after_write(2);
    let done = reclaim_whole_with(&t, &watch(now), now, DAY, &mut || Ok(()), &mut |_, _| {})
        .expect("goes whole");
    assert_eq!(done.units, std::slice::from_ref(&t), "{done:?}");
}

/// A CLONE OF THE MEASURED VOLUME IS REFUSED BY ITS GUID: a build directory whose handle
/// reads the measured serial but another volume GUID ([`mark_cloned`]: a sector-cloned disk or
/// an attached VHD copy keeps its source's serial) is refused by every tier before anything
/// is opened under it, and so is any build directory while the measured volume's GUID is
/// unread; nothing is touched. The same judge reclaims an unmarked one on the measured volume.
/// NEGATIVE CONTROL: without the GUID check every tier would reclaim the clone's.
#[test]
fn a_clone_of_the_measured_volume_is_refused_by_its_guid() {
    let tmp = Tmp::new("win-cloned-volume");
    let home = PathBuf::from(plain(tmp.path()));
    let device = dev_of(&home).expect("the measured volume");
    let volume = VolumeGuid::of_path_now(&home);
    assert!(volume.is_some());
    let watch = |now: i64| Judge {
        device: Some(device),
        volume,
        ..judge(now)
    };
    let unread = |now: i64| Judge {
        volume: None,
        ..watch(now)
    };
    let t = synthetic_target(&tmp, "cloned");
    let debug = t.join("debug");
    mark_cloned(&t);
    let before = snapshot(&t, &|_| false);
    let now = now_after_write(STALE + 1);
    for (judged, why) in [
        (watch(now), "GUIDs differ"),
        (unread(now), "measured volume's own name"),
    ] {
        for (tier, refused) in [
            ("tier 2", reclaim(&t, &judged)),
            (
                "tier 4",
                reclaim_lru(&t, &debug, &judged, now - STALE * DAY),
            ),
            (
                "tier 1",
                reclaim_whole_with(&t, &judged, now, DAY, &mut || Ok(()), &mut |_, _| {}),
            ),
            (
                "tier 3",
                reclaim_profiles(&t, &judged, now, DAY, &mut || Ok(()), &mut || Ok(())),
            ),
        ] {
            let err = refused.expect_err(tier).to_string();
            assert!(err.contains(why), "{tier}: {err}");
        }
    }
    assert_eq!(snapshot(&t, &|_| false), before, "nothing touched");

    let t = synthetic_target(&tmp, "measured");
    let done = reclaim(&t, &watch(now)).expect("the measured volume's: reclaimed");
    assert_eq!(done.units, vec![at(&t, "debug/incremental")], "{done:?}");
}

/// Every file and folder at or under `root`, folders before what they hold.
fn tree_of(root: &Path) -> Vec<PathBuf> {
    let mut out = vec![root.to_path_buf()];
    let mut i = 0;
    while let Some(dir) = out.get(i).cloned() {
        i += 1;
        if std::fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir()) {
            let mut children: Vec<PathBuf> = std::fs::read_dir(&dir)
                .expect("listed")
                .map(|e| e.expect("entry").path())
                .collect();
            children.sort();
            out.extend(children);
        }
    }
    out
}

/// A BUILD DIRECTORY IN A REAL CLOUD SYNC ROOT LOSES NOTHING. A scratch folder registered as
/// a cloud sync root ([`aterm_dirfd::seam::SyncRoot`], as OneDrive registers its folder, with
/// Documents and Desktop moved into it) holds a checkout's idle build directory, every file
/// and folder of it made an in-sync placeholder, as a sync client leaves what it uploaded.
/// This test thread runs in the process's default placeholder mode, which shows each of them
/// as a plain file or folder — yet the report's own walk of it cannot open it, so it is no
/// candidate, every tier refuses it before anything is opened under it (the sync root on its
/// path is a reparse point that is no link), and every byte stays: a delete there would remove
/// the files from the cloud and every synced device. The report of it, named by hand, and
/// every refusal say it lies in a cloud sync root ([`IN_SYNC_ROOT`]), not merely that
/// something on its path is a link (`disk_target_tests`' link cases say that, and never a sync
/// root). Unregistered on drop, after which it is a plain tree. NEGATIVE CONTROL: with
/// `aterm-dirfd`'s placeholders left disguised, the same tree is reclaimed.
#[test]
fn a_build_directory_in_a_cloud_sync_root_loses_nothing() {
    let tmp = Tmp::new("win-sync-root");
    let sync = tmp.path().join("synced");
    let t = lay_target(&sync.join("proj").join("target"));
    let t = std::fs::canonicalize(&t).expect("canonical");
    let debug = t.join("debug");
    let root = aterm_dirfd::seam::SyncRoot::register(&sync).expect("a sync root");
    for made in tree_of(&sync.join("proj")) {
        root.convert(&made).expect("a placeholder");
    }
    assert_eq!(aterm_dirfd::seam::thread_placeholder_mode(), 0);
    let before = snapshot(&t, &|_| false);
    let now = now_after_write(STALE + 1);
    let report = assessed(&t, now).fences().expect_err("no candidate");
    assert!(report.contains(IN_SYNC_ROOT), "{report}");
    for (tier, refused) in [
        ("tier 2", reclaim(&t, &judge(now))),
        (
            "tier 4",
            reclaim_lru(&t, &debug, &judge(now), now - STALE * DAY),
        ),
        (
            "tier 1",
            reclaim_whole_with(&t, &judge(now), now, DAY, &mut || Ok(()), &mut |_, _| {}),
        ),
        (
            "tier 3",
            reclaim_profiles(&t, &judge(now), now, DAY, &mut || Ok(()), &mut || Ok(())),
        ),
    ] {
        let err = refused.expect_err(tier).to_string();
        assert!(err.contains(IN_SYNC_ROOT), "{tier}: {err}");
    }
    assert_eq!(snapshot(&t, &|_| false), before, "every synced byte stays");
    assert_eq!(aterm_dirfd::seam::thread_placeholder_mode(), 0);

    // NEGATIVE CONTROL: disguised, the sync root is a plain folder and the cache goes.
    aterm_dirfd::seam::leave_placeholders_disguised(true);
    let done = reclaim(&t, &judge(now));
    aterm_dirfd::seam::leave_placeholders_disguised(false);
    let done = done.expect("disguised: reclaimed");
    assert_eq!(done.units, vec![at(&t, "debug/incremental")], "{done:?}");
    drop(root);
}

/// One path, any spelling (the one path-identity rule): the pressure pass's pick named by
/// its plain, upper-cased spelling is the profile the walk found by its verbatim one, and is
/// taken; a build directory named inside, to be left to its own row, is left whatever its
/// spelling; one that cannot be placed leaves every profile it might hold — here all.
#[test]
fn a_profile_or_a_nested_build_directory_is_one_path_in_any_spelling() {
    let tmp = Tmp::new("win-spelling");
    let t = synthetic_target(&tmp, "target");
    let debug = t.join("debug");
    let now = now_after_write(0);
    last_used(&debug, now - 3 * 3_600);

    let done = reclaim_lru(&t, &shouted(&debug), &judge(now), now - 3 * 3_600)
        .expect("the pick, in another spelling");
    assert_eq!(done.units, vec![debug.join(INCREMENTAL)], "{done:?}");

    let t = synthetic_target(&tmp, "outer");
    let nested = lay_target(&at(&t, "nested/target"));
    let later = now_after_write(STALE + 1);
    let done = reclaim_leaving(&t, &judge(later), &[shouted(&nested)]).expect("its own profile");
    assert_eq!(done.units, vec![at(&t, "debug/incremental")], "{done:?}");
    assert!(
        at(&nested, "debug/incremental").exists(),
        "the nested directory's profile is left"
    );

    let t = synthetic_target(&tmp, "unplaced");
    let nested = lay_target(&at(&t, "nested/target"));
    let err = reclaim_leaving(&t, &judge(later), &[PathBuf::from(r"nested\target")])
        .expect_err("nothing placed outside it");
    assert!(err.to_string().contains("nothing reclaimed"), "{err}");
    assert!(at(&t, "debug/incremental").exists());
    assert!(at(&nested, "debug/incremental").exists());
}

/// Tier 3 empties a profile's uplifts that are cargo's: an unhashed twin of a `deps\` entry
/// that is the same file (a hard link) or holds the same bytes goes; one whose bytes differ,
/// and a file cargo never writes, stay and are said; the locks and the profile stay.
#[test]
fn tier_3_takes_an_unhashed_uplift_only_when_its_twin_is_the_same_artifact() {
    let tmp = Tmp::new("win-uplift");
    let t = synthetic_target(&tmp, "target");
    let debug = t.join("debug");
    let deps = debug.join("deps");
    std::fs::write(deps.join("lay.pdb"), vec![1u8; 32 * 1024]).expect("pdb");
    std::fs::hard_link(deps.join("lay.pdb"), debug.join("lay.pdb")).expect("linked uplift");
    std::fs::write(deps.join("lay_two.pdb"), vec![2u8; 16 * 1024]).expect("twin");
    std::fs::write(debug.join("lay-two.pdb"), vec![2u8; 16 * 1024]).expect("copied uplift");
    std::fs::write(deps.join("differs.pdb"), vec![3u8; 4096]).expect("twin");
    std::fs::write(debug.join("differs.pdb"), vec![4u8; 4096]).expect("not its twin");
    std::fs::write(debug.join("notes.txt"), b"mine").expect("not cargo's");
    let now = now_after_write(8);

    let done = reclaim_profiles(
        &t,
        &judge(now),
        now,
        7 * DAY,
        &mut || Ok(()),
        &mut || Ok(()),
    )
    .expect("emptied");
    assert!(done.units.contains(&debug), "{done:?}");
    for gone in [
        "lay.pdb",
        "lay-two.pdb",
        "deps",
        "incremental",
        ".fingerprint",
    ] {
        assert!(
            std::fs::symlink_metadata(debug.join(gone)).is_err(),
            "{gone} went: {done:?}"
        );
    }
    for kept in ["differs.pdb", "notes.txt"] {
        assert!(debug.join(kept).exists(), "{kept} stays");
        assert!(
            done.skipped.iter().any(|(p, _)| p == &debug.join(kept)),
            "{kept} said: {done:?}"
        );
    }
    assert!(CARGO_LOCKS.iter().all(|l| debug.join(l).exists()));
}

/// A MEASUREMENT, run by hand (`-- --ignored --nocapture reclaim_wall_time`), never a gate:
/// the wall time of the report walks and of each reclaim over a synthetic build directory of
/// a realistic size — 20,000 files of 4 KiB (10,000 in `incremental/`, 5,000 in `deps/`,
/// 5,000 under `.fingerprint/`). It asserts only what went.
#[test]
#[ignore = "a measurement, run by hand"]
fn reclaim_wall_time() {
    let tmp = Tmp::new("win-measure");
    let t = synthetic_target(&tmp, "target");
    let debug = t.join("debug");
    let body = vec![5u8; 4096];
    let laid = std::time::Instant::now();
    for s in 0..100 {
        let d = at(&debug, &format!("incremental/lay-{s:03}/s-session"));
        std::fs::create_dir_all(&d).expect("session");
        for f in 0..100 {
            std::fs::write(d.join(format!("q{f:03}.bin")), &body).expect("cache");
        }
    }
    for f in 0..5_000 {
        std::fs::write(at(&debug, &format!("deps/lib{f:04}.rmeta")), &body).expect("dep");
    }
    for u in 0..1_000 {
        let d = at(&debug, &format!(".fingerprint/unit-{u:04}"));
        std::fs::create_dir_all(&d).expect("unit");
        for f in 0..5 {
            std::fs::write(d.join(format!("f{f}")), &body).expect("fingerprint");
        }
    }
    eprintln!("laid 20,000 files in {:?}", laid.elapsed());
    let now = now_after_write(STALE + 1);
    let time = |what: &str, f: &mut dyn FnMut()| {
        let started = std::time::Instant::now();
        f();
        eprintln!("{what}: {:?}", started.elapsed());
    };
    time(
        "assess (profile walk, lock probe, incremental/ size)",
        &mut || {
            let td = assessed(&t, now);
            assert!(td.reclaimable(now, STALE).0 > 0);
        },
    );
    time(
        "total_bytes_owned (every entry's size and owner)",
        &mut || {
            let ((bytes, partial), foreign) = total_bytes_owned(&t);
            assert!(bytes > 0 && !partial && foreign.is_none());
        },
    );
    time("foreign_beyond (every entry's owner)", &mut || {
        assert_eq!(foreign_beyond(&t), None);
    });
    time("tier 2: reclaim incremental/ (10,000 files)", &mut || {
        let done = reclaim(&t, &judge(now)).expect("reclaimed");
        assert_eq!(done.units, vec![at(&t, "debug/incremental")]);
    });
    time("tier 1: whole removal (10,000 files left)", &mut || {
        let done = reclaim_whole_with(&t, &judge(now), now, DAY, &mut || Ok(()), &mut |_, _| {})
            .expect("goes whole");
        assert_eq!(done.units, std::slice::from_ref(&t));
    });
    assert!(!t.exists());
}
