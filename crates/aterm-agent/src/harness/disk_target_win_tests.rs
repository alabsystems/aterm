// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Windows tests for the build-directory reclaim's Windows half.

use super::*;
use crate::harness::disk::discover;
use std::os::windows::fs::OpenOptionsExt as _;

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

fn among(
    pids: Vec<u32>,
) -> impl Fn(&[PathBuf], &[u32]) -> std::io::Result<aterm_uds::holders::PathScan> {
    move |dirs, skip| aterm_uds::holders::scan_path_users_of(&pids, dirs, skip)
}

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

const FOREIGN_TAG: u32 = 0x0000_7A7E;

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

const NO_VOLUME: &str = r"\??\Volume{00000000-0000-0000-0000-000000000000}\";

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

fn shouted(p: &Path) -> PathBuf {
    PathBuf::from(plain(p).to_uppercase())
}

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

    aterm_dirfd::seam::leave_placeholders_disguised(true);
    let done = reclaim(&t, &judge(now));
    aterm_dirfd::seam::leave_placeholders_disguised(false);
    let done = done.expect("disguised: reclaimed");
    assert_eq!(done.units, vec![at(&t, "debug/incremental")], "{done:?}");
    drop(root);
}

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
