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
            if stage == Stage::Staging {
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

/// A build directory whose folders and files are a sync engine's placeholders loses nothing.
/// Exposed, as every `aterm_dirfd` call exposes them, the placeholders fence it: no tier
/// reclaims, each says why, and every synced byte stays. Left in the mode a program that never
/// asks runs in (`leave_placeholders_disguised`), Windows shows a placeholder as a plain folder
/// through a handle on it and in a placeholder folder's listing, so the pass gets past every
/// fence that stopped it and stages `incremental/`. A plain folder's listing, though, the
/// stage's, still names a placeholder in it a reparse point (`FILE_ATTRIBUTE_REPARSE_POINT`,
/// the cloud tag), in every listing class and in std's: the stage's delete keeps it, it goes
/// back where it was, and still every byte stays.
///
/// Negative control: a delete that judged a listed entry by its handle rather than by the
/// listing takes `incremental/` there (the disguised pass then answers it reclaimed).
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
    let err = done
        .expect_err("disguised: the stage's listing names the placeholder")
        .to_string();
    assert!(!err.contains(IN_SYNC_ROOT), "past every fence: {err}");
    let kept = format!(
        "{}: {}",
        at(&t, "debug/incremental").display(),
        std::io::Error::from(aterm_dirfd::Errno::FOREIGN_REPARSE)
    );
    assert!(err.contains(&kept), "{err}");
    assert_eq!(
        snapshot(&t, &|_| false),
        before,
        "every synced byte stays, back where it was"
    );
    assert_eq!(aterm_dirfd::seam::thread_placeholder_mode(), 0);
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

/// One removal of each tier on the synthetic target `t` at `now` (`seen` its survey): 0 to 3 as
/// [`ordinary_method`] names them (the idle reclaim, the pressure reclaim, the whole removal,
/// tier 3), and 4 tier 0's, of the died build's metadata folder `debug/deps/rmetaAb12Cd`.
fn windows_method(
    method: usize,
    t: &Path,
    now: i64,
    seen: i64,
    hook: &mut dyn FnMut(Stage, &Path),
) -> std::io::Result<Removed> {
    if method < 4 {
        return ordinary_method(method, t, now, seen, hook);
    }
    let debug = t.join("debug");
    let piece = at(&debug, "deps/rmetaAb12Cd");
    reclaim_died_with(t, &debug, &piece, &judge(now), seen, &mut || Ok(()), hook)
}

/// Lay the died build's metadata folder [`windows_method`]'s tier 0 takes, under `t`'s profile.
fn lay_died_piece(t: &Path) -> PathBuf {
    let piece = at(t, "debug/deps/rmetaAb12Cd");
    std::fs::create_dir_all(&piece).expect("rmeta");
    std::fs::write(piece.join("lib.rmeta"), vec![2u8; 8192]).expect("rmeta file");
    piece
}

/// The exit test of DC-20w: on Windows each remover of the disk pass (tiers 0 to 4) takes what it
/// removes under cargo's locks, moves it into a stage of its own, lets the locks go, and only
/// then deletes the stage, so a build that starts in that profile meanwhile takes its lock at
/// once. NTFS renames no folder while a file under it is open, and the pass holds each
/// profile's locks open inside it, so the whole removal stages one level lower: the profile, its
/// locks, and a build that took one stay, and the directory is not gone whole on that pass.
///
/// Negative controls: the probe at `Staged` finds the lock held, and a pass that deleted in
/// place would come to its first removal with `debug/.cargo-lock` held, where `Build::holds`
/// panics; the whole removal that went on to remove the profile its build holds would leave no
/// `debug/.cargo-lock`.
#[test]
fn a_pass_on_windows_lets_go_of_the_cargo_locks_before_it_deletes() {
    for method in 0..5 {
        let tmp = Tmp::new("win-lets-go");
        let t = synthetic_target(&tmp, "target");
        let debug = t.join("debug");
        let piece = if method == 4 {
            lay_died_piece(&t)
        } else {
            PathBuf::new()
        };
        let now = now_after_write(STALE + 1);
        let seen = if method == 4 {
            now
        } else {
            assessed(&t, now).profiles[0].last_used.unwrap()
        };
        let lock = debug.join(".cargo-lock");
        let mut order = Vec::new();
        let mut held_when_staged = None;
        let mut staged_in: Vec<PathBuf> = Vec::new();
        let mut free_at_first_removal = None;
        let mut build: Option<Build> = None;
        let done = windows_method(method, &t, now, seen, &mut |stage, folder| {
            if stage != Stage::Removing {
                if stage == Stage::Staged {
                    if held_when_staged.is_none() {
                        held_when_staged = Some(held(&lock));
                    }
                    staged_in.push(folder.to_path_buf());
                }
                order.push(stage);
                return;
            }
            if build.is_none() {
                free_at_first_removal = Some(!held(&lock));
                build = Some(Build::holds(&lock));
            }
        })
        .expect("reclaimed");
        // The whole removal stages each pinned folder's entries, and deletes each stage.
        order.dedup();
        assert_eq!(order, STAGED_ORDER, "method {method}");
        assert_eq!(held_when_staged, Some(true), "method {method}");
        assert_eq!(free_at_first_removal, Some(true), "method {method}");
        assert!(build.is_some(), "method {method}: a build took the lock");
        assert!(done.bytes > 0, "method {method}: {done:?}");
        assert!(stages_in(&t).is_empty(), "method {method}: stage left");
        match method {
            0 | 1 => assert!(!debug.join(INCREMENTAL).exists(), "method {method}"),
            2 => {
                // Each folder it stages in, spelled as its lock and its lines spell it: the
                // directory itself is the path it was given, no separator after.
                let spelled: Vec<&std::ffi::OsStr> =
                    staged_in.iter().map(|p| p.as_os_str()).collect();
                assert_eq!(spelled, [t.as_os_str(), debug.as_os_str()], "{done:?}");
                assert!(t.join("CACHEDIR.TAG").exists(), "{done:?}");
                assert!(lock.exists(), "the build's lock stays: {done:?}");
                assert!(!debug.join("deps").exists(), "{done:?}");
                assert!(done.units.is_empty(), "{done:?}");
                assert!(
                    done.trouble.iter().any(|w| w.contains("a build holds")),
                    "{done:?}"
                );
            }
            3 => {
                for gone in ["deps", "build", ".fingerprint", INCREMENTAL] {
                    assert!(!debug.join(gone).exists(), "{gone}: {done:?}");
                }
            }
            _ => {
                assert!(!piece.exists(), "{done:?}");
                assert_eq!(done.units, vec![piece.clone()], "{done:?}");
            }
        }
        drop(build);
    }
}

/// On Windows an entry a handle holds open (here a test binary running from `deps/`: its file
/// open under the folder) refuses its rename into the stage. It stays where it was, whole: never
/// deleted in place under the locks instead. The reason is said, the build directory is not
/// gone whole, and the next pass, the handle closed, takes it (tiers 1 and 3, the ones that
/// take `deps/`).
///
/// Negative control: `deps/liblay.rlib`, beside the open file, survives the first pass; a pass
/// that fell back to deleting the refused entry in place would remove it, and one that took the
/// open file's siblings would leave `deps/` without it.
#[test]
fn a_pass_on_windows_keeps_an_entry_an_open_handle_holds_for_the_next_pass() {
    for method in [2, 3] {
        let tmp = Tmp::new("win-held-entry");
        let t = synthetic_target(&tmp, "target");
        let debug = t.join("debug");
        let running = at(&debug, "deps/lay-0123abcd.exe");
        std::fs::write(&running, vec![1u8; 16 * 1024]).expect("a test binary");
        let now = now_after_write(8);
        // Shared for reading alone, as a loaded program image is.
        let open = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&running)
            .expect("running");
        let done = windows_method(method, &t, now, now, &mut |_, _| {}).expect("the rest went");
        assert!(running.exists(), "method {method}: {done:?}");
        assert!(
            at(&debug, "deps/liblay.rlib").exists(),
            "method {method}: deps/ is kept whole: {done:?}"
        );
        assert!(
            !debug.join(INCREMENTAL).exists(),
            "method {method}: {done:?}"
        );
        assert!(
            done.trouble.iter().any(|w| w.contains("deps")
                && w.contains("a handle holds it open")
                && w.contains("stays where it was")),
            "method {method}: said: {done:?}"
        );
        assert!(!done.units.contains(&t), "method {method}: {done:?}");
        assert!(t.exists(), "method {method}");
        assert!(stages_in(&t).is_empty(), "method {method}: no stage left");
        drop(open);

        // The next pass, the whole removal, takes what stayed and the directory with it.
        let again = windows_method(2, &t, now, now, &mut |_, _| {}).expect("the next pass");
        assert_eq!(
            again.units,
            std::slice::from_ref(&t),
            "method {method}: {again:?}"
        );
        assert!(!t.exists(), "method {method}");
    }
}

/// On Windows a whole removal that can stage nothing more after an entry a handle held stayed
/// (here a full disk at the rename after `deps/`'s) deletes the rest in place under the locks,
/// as where no stage can be made, but never what the handle held: `deps/` stays whole and the
/// reason is still said.
///
/// Negative control: `deps/liblay.rlib`, beside the open file, survives; the fallback that
/// deleted the whole build directory in place would remove it and lose the handle's line.
#[test]
fn a_whole_removal_that_falls_back_in_place_keeps_what_a_handle_held() {
    let tmp = Tmp::new("win-held-fallback");
    let t = synthetic_target(&tmp, "target");
    let debug = t.join("debug");
    let running = at(&debug, "deps/lay-0123abcd.exe");
    std::fs::write(&running, vec![1u8; 16 * 1024]).expect("a test binary");
    let now = now_after_write(8);
    let open = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&running)
        .expect("running");
    // `.rustc_info.json`, `.fingerprint`, `build` and `deps` are tried in that order; the
    // fifth rename (`incremental`) finds the disk full.
    fd::inject_move_fault(Some((4, 112)));
    let result = windows_method(2, &t, now, now, &mut |_, _| {});
    fd::inject_move_fault(None);
    let done = result.expect("the rest went");
    assert!(running.exists(), "{done:?}");
    assert!(at(&debug, "deps/liblay.rlib").exists(), "{done:?}");
    for gone in [".fingerprint", "build", INCREMENTAL] {
        assert!(!debug.join(gone).exists(), "{gone}: {done:?}");
    }
    assert!(
        done.trouble
            .iter()
            .any(|w| w.contains("deps") && w.contains("a handle holds it open")),
        "{done:?}"
    );
    assert!(done.units.is_empty(), "{done:?}");
    assert!(t.exists());
    assert!(stages_in(&t).is_empty(), "no stage left");
    drop(open);
}

/// On Windows a pass killed once it staged and let go (here a panic at its first removal)
/// leaves each stage it made tagged and let go, and the next pass, the directory's idle row,
/// finishes every one and counts what it held, as on unix.
///
/// Negative control: the same pass without the panic leaves no stage.
#[test]
fn a_pass_on_windows_finishes_a_stage_a_killed_pass_left() {
    for method in 0..5 {
        for stop in [true, false] {
            let tmp = Tmp::new("win-left-stage");
            let t = synthetic_target(&tmp, "target");
            if method == 4 {
                lay_died_piece(&t);
            }
            let now = now_after_write(STALE + 1);
            let seen = if method == 4 {
                now
            } else {
                assessed(&t, now).profiles[0].last_used.unwrap()
            };
            let first = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                windows_method(method, &t, now, seen, &mut |stage, _| {
                    assert!(
                        !(stop && stage == Stage::Removing),
                        "the pass stops at its first removal"
                    );
                })
            }));
            let stages = stages_in(&t);
            if !stop {
                first.expect("no stop").expect("reclaimed");
                assert!(stages.is_empty(), "method {method}: {stages:?}");
                continue;
            }
            assert!(first.is_err(), "method {method}: the pass stopped");
            // The whole removal stages the root's entries and the profile's, each in its own.
            let made = if method == 2 { 2 } else { 1 };
            assert_eq!(stages.len(), made, "method {method}: {stages:?}");
            for stage in &stages {
                assert!(stage.join(STAGE_TAG).exists(), "method {method}");
                assert!(!held(&stage.join(STAGE_TAG)), "method {method}");
            }
            let td = assessed(&t, now);
            assert_eq!(td.stages.len(), made, "method {method}: {td:?}");
            let done = reclaim(&t, &judge(now)).expect("the next pass finishes it");
            assert!(done.bytes > 0, "method {method}: {done:?}");
            for stage in &stages {
                assert!(done.units.contains(stage), "method {method}: {done:?}");
            }
            assert!(stages_in(&t).is_empty(), "method {method}");
        }
    }
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
