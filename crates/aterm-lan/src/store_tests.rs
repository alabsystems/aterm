// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

use super::*;

/// The one-minute hang detector a test waits on a walk with.
const MUST_HAPPEN: Duration = Duration::from_secs(60);

/// A store reached by two mount points is counted once, a link is counted as itself, a folder
/// the walk cannot read marks the answer partial, and the direct path answers no store.
#[test]
fn a_walk_counts_each_store_once_follows_no_link_and_says_what_it_could_not_read() {
    let root = aterm_tempfile::tempdir().unwrap();
    let store = root.path().join(STORE_NAME);
    std::fs::create_dir_all(store.join("Store-V2")).unwrap();
    std::fs::write(store.join("Store-V2").join("shard"), vec![3u8; 50_000]).unwrap();
    std::os::unix::fs::symlink(root.path(), store.join("loop")).unwrap();
    let twice = vec![
        ("/".to_owned(), root.path().to_path_buf()),
        ("/System/Volumes/Data".to_owned(), root.path().to_path_buf()),
    ];
    let read = walk(twice, Bounds::BROKER);
    assert_eq!(read.len(), 1, "{read:?}");
    assert_eq!((read[0].entries, read[0].partial), (4, None), "{read:?}");
    let link = std::fs::symlink_metadata(store.join("loop")).unwrap();
    let shard = std::fs::metadata(store.join("Store-V2").join("shard")).unwrap();
    assert!(read[0].bytes >= shard.blocks() * 512 + link.blocks() * 512);

    let shut = store.join("Store-V2");
    std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o000)).unwrap();
    let read = walk(
        vec![("/".to_owned(), root.path().to_path_buf())],
        Bounds::BROKER,
    );
    std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o755)).unwrap();
    // Root reads every folder: only an unprivileged run sees the refusal.
    if aterm_uds::peer::our_uid() != 0 {
        assert_eq!(read[0].partial, Some(StoreBound::Unreadable), "{read:?}");
    }

    // A store folder that is a link is no store, whatever it points at.
    let linked = aterm_tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(&store, linked.path().join(STORE_NAME)).unwrap();
    let read = walk(
        vec![("/Volumes/Linked".to_owned(), linked.path().to_path_buf())],
        Bounds::BROKER,
    );
    assert_eq!(read, [], "a linked store is not walked");

    let counts = Counts::new();
    let (code, _) = Store::Unserved.answer(&counts).unwrap_err();
    assert_eq!(code, libc::ENOTSUP);
    assert_eq!(counts.get(Count::StoreWalks), 0);
}

/// A folder swapped after the walk listed it, for a link to a folder outside the store or for
/// another folder moved into its name, is not entered: the walk opens each folder by descriptor
/// from the folder that listed it, never through a link, and only while it is the folder it
/// counted. CONTROL: with nothing swapped, the same folder is entered and its files counted.
#[test]
fn a_folder_swapped_after_it_was_listed_is_not_entered() {
    let fixture = || {
        let root = aterm_tempfile::tempdir().unwrap();
        let store = root.path().join(STORE_NAME);
        std::fs::create_dir_all(store.join("a")).unwrap();
        std::fs::write(store.join("a").join("one"), vec![1u8; 4096]).unwrap();
        std::fs::write(store.join("a").join("two"), vec![2u8; 4096]).unwrap();
        let outside = root.path().join("outside");
        std::fs::create_dir_all(outside.join("deeper")).unwrap();
        for k in 0..20 {
            std::fs::write(outside.join(format!("secret{k}")), vec![9u8; 65_536]).unwrap();
        }
        (root, store, outside)
    };
    let walk_swapping = |swap: &dyn Fn(&Path, &Path)| {
        let (root, store, outside) = fixture();
        let mut first = true;
        let read = walk_between(
            vec![("/Volumes/Fixture".to_owned(), root.path().to_path_buf())],
            Bounds::BROKER,
            &mut || {
                if std::mem::take(&mut first) {
                    std::fs::rename(store.join("a"), root.path().join("a.was")).unwrap();
                    swap(&store, &outside);
                }
            },
        );
        assert_eq!(read.len(), 1, "{read:?}");
        (read[0].entries, read[0].bytes)
    };

    let (entries, _) = walk_swapping(&|store, _| {
        std::fs::rename(store.parent().unwrap().join("a.was"), store.join("a")).unwrap();
    });
    assert_eq!(entries, 4, "CONTROL: the store, a and a's two files");

    let (entries, bytes) = walk_swapping(&|store, outside| {
        std::os::unix::fs::symlink(outside, store.join("a")).unwrap();
    });
    assert_eq!(entries, 2, "a link in a's place is not followed");
    assert!(bytes < 20 * 65_536, "{bytes}");

    let (entries, bytes) = walk_swapping(&|store, outside| {
        std::fs::rename(outside, store.join("a")).unwrap();
    });
    assert_eq!(
        entries, 2,
        "another folder in a's name is not the folder counted"
    );
    assert!(bytes < 20 * 65_536, "{bytes}");
}

/// A walk whose read never returns holds its own thread only: each request waits for it at most
/// the walk's bound and [`WAIT_PAST_WALK`], is answered `ETIMEDOUT` (no walk was kept before),
/// and starts no second walk; once the read returns, its walk answers. A mount table read short
/// is answered `EIO`, never as a store list missing some volumes.
#[test]
fn a_stuck_walk_holds_no_request_and_a_short_mount_table_is_no_answer() {
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let root = aterm_tempfile::tempdir().unwrap();
    let roots: Roots = {
        let gate = Arc::clone(&gate);
        let mount = root.path().to_path_buf();
        Box::new(move || {
            let (open, opened) = &*gate;
            let open = open.lock().unwrap();
            drop(opened.wait_while(open, |open| !*open).unwrap());
            Some(vec![("/Volumes/Stuck".to_owned(), mount.clone())])
        })
    };
    let stuck = Store::walked(
        roots,
        Bounds {
            entries: MAX_ENTRIES,
            time: Duration::from_millis(10),
        },
        Box::new(Instant::now),
    );
    let counts = Counts::new();
    for _ in 0..2 {
        let (code, _) = stuck.answer(&counts).unwrap_err();
        assert_eq!(code, libc::ETIMEDOUT);
    }
    assert_eq!(counts.get(Count::StoreWalks), 1, "one walk runs at a time");

    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    let Store::Walked(walker) = &stuck else {
        unreachable!("a walker serves")
    };
    let (walks, waited) = walker
        .ended
        .wait_timeout_while(walker.lock(), MUST_HAPPEN, |s| s.walking)
        .unwrap();
    assert!(!waited.timed_out(), "the walk ends once its read returns");
    drop(walks);
    let answer = stuck.answer(&counts).unwrap();
    assert_eq!(answer.volumes, [], "an empty volume holds no store");
    assert_eq!(counts.get(Count::StoreWalks), 1, "the kept walk answers");

    let short = Store::walked(Box::new(|| None), Bounds::BROKER, Box::new(Instant::now));
    let (code, _) = short.answer(&counts).unwrap_err();
    assert_eq!(code, libc::EIO);
}
