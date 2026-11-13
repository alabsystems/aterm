// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn reordered_browse_events_do_not_hide_later_peers() {
    const DEFAULT: &str = "http://default.local:7484/v1/alab";
    const PRIMARY: &str = "http://primary.local:7484/v1/alab";
    let names: Vec<_> = (0..35).map(|n| format!("node-{n:02}")).collect();
    let expected_names: BTreeSet<_> = names.iter().cloned().collect();
    let expected_urls: BTreeSet<_> = std::iter::once(DEFAULT.to_owned())
        .chain(
            names
                .iter()
                .map(|name| format!("http://{name}.local:7484/v1/alab")),
        )
        .collect();
    let limit = MAX_CANDIDATES - 1;
    for saved in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(root.path());
        fsx::ensure_private_dir(&paths.dir).unwrap();
        let memory = AtomicUsize::new(0);
        let mut queried = BTreeSet::new();
        let mut reached = BTreeSet::new();
        for pass in 0..names.len().div_ceil(limit) {
            // A new browse can hear exactly the same services in another
            // order. This order would cancel a position-only rotation.
            let mut arrivals = names.clone();
            arrivals.rotate_right((pass * limit) % names.len());
            let heard = arrivals.clone();
            let fresh = AtomicUsize::new(0);
            let plan = Plan::new(
                "alab",
                PRIMARY,
                &[DEFAULT],
                &[],
                &[],
                Pool::Names(heard),
                Cursor {
                    memory: if saved { &fresh } else { &memory },
                    saved: saved.then_some((&paths, PeerUse::Reader)),
                },
            );
            let urls = plan.finish("alab", PRIMARY, Duration::from_secs(1), |selected, _| {
                assert!(selected.len() <= limit);
                queried.extend(selected.iter().cloned());
                selected
                    .iter()
                    .map(|name| DiscoveredVault {
                        url: format!("http://{name}.local:7484/v1/alab"),
                        txt: None,
                    })
                    .collect()
            });
            assert!(urls.len() <= MAX_CANDIDATES);
            assert_eq!(urls.first().map(String::as_str), Some(DEFAULT));
            reached.extend(urls);
        }
        assert_eq!(queried, expected_names, "saved={saved}");
        assert_eq!(reached, expected_urls, "saved={saved}");
    }
}

#[test]
fn one_shot_cursors_are_per_home_and_skip_unneeded_storage() {
    let root = aterm_tempfile::tempdir().unwrap();
    let left = Paths::for_home(&root.path().join("left"));
    let right = Paths::for_home(&root.path().join("right"));
    for paths in [&left, &right] {
        fsx::ensure_private_dir(&paths.dir).unwrap();
    }
    let select = |paths: &Paths, purpose, count| {
        let memory = AtomicUsize::new(0);
        Cursor {
            memory: &memory,
            saved: Some((paths, purpose)),
        }
        .select((0..count).collect::<Vec<_>>(), MAX_CANDIDATES)
    };
    assert_eq!(select(&left, PeerUse::Records, 2), vec![0, 1]);
    assert!(!left.dir.join(CURSOR_FILE).exists());
    assert!(!left.dir.join(CURSOR_LOCK).exists());
    for purpose in [PeerUse::Records, PeerUse::Envelope, PeerUse::Reader] {
        assert_eq!(select(&left, purpose, 35), (0..16).collect::<Vec<_>>());
    }
    assert_eq!(
        select(&right, PeerUse::Records, 35),
        (0..16).collect::<Vec<_>>()
    );
    assert_eq!(
        select(&left, PeerUse::Records, 35),
        (16..32).collect::<Vec<_>>()
    );
}

#[test]
fn join_rotation_preserves_the_cursor_file_an_older_reader_uses() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(root.path());
    fsx::ensure_private_dir(&paths.dir).unwrap();
    let original: Vec<u8> = b"APC1"
        .iter()
        .copied()
        .chain([5_u64, 9, 13].into_iter().flat_map(u64::to_le_bytes))
        .collect();
    let path = paths.dir.join(CURSOR_FILE);
    fsx::write_atomic(&path, &original, 0o600).unwrap();
    assert_eq!(saved_start(&paths, PeerUse::Join, 35, 16), Some(0));
    assert_eq!(saved_start(&paths, PeerUse::Queue, 35, 16), Some(0));
    assert_eq!(
        fsx::read_private(&path, 28).unwrap().unwrap().as_slice(),
        original
    );
    assert_eq!(saved_start(&paths, PeerUse::Reader, 35, 16), Some(13));
    assert_eq!(saved_start(&paths, PeerUse::Join, 35, 16), Some(16));
    assert_eq!(saved_start(&paths, PeerUse::Queue, 35, 16), Some(16));
    let updated = fsx::read_private(&path, 28).unwrap().unwrap();
    assert_eq!(&updated.as_slice()[..20], &original[..20]);
    assert_eq!(&updated.as_slice()[20..], &29_u64.to_le_bytes());
}

#[test]
fn one_shot_cursor_falls_back_when_hint_is_busy_or_damaged() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(root.path());
    let held = StateLock::take_at(&paths, &paths.dir.join(CURSOR_LOCK)).unwrap();
    let other = paths.clone();
    let (send, receive) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let memory = AtomicUsize::new(0);
        let cursor = Cursor {
            memory: &memory,
            saved: Some((&other, PeerUse::Records)),
        };
        send.send([
            cursor.select((0..35).collect::<Vec<_>>(), MAX_CANDIDATES),
            cursor.select((0..35).collect::<Vec<_>>(), MAX_CANDIDATES),
        ])
        .unwrap();
    });
    let observed = receive.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
    drop(held);
    worker.join().unwrap();
    let expected = [(0..16).collect::<Vec<_>>(), (16..32).collect::<Vec<_>>()];
    assert_eq!(observed.unwrap(), expected);

    let file = paths.dir.join(CURSOR_FILE);
    fsx::write_atomic(&file, b"damaged", 0o600).unwrap();
    let memory = AtomicUsize::new(0);
    let cursor = Cursor {
        memory: &memory,
        saved: Some((&paths, PeerUse::Records)),
    };
    for expected in expected {
        assert_eq!(
            cursor.select((0..35).collect::<Vec<_>>(), MAX_CANDIDATES),
            expected
        );
    }
    assert_eq!(
        fsx::read_private(&file, 28).unwrap().unwrap().as_slice(),
        b"damaged"
    );
}

#[test]
fn named_and_known_candidates_share_the_actual_final_url_budget() {
    const DEFAULT: &str = "http://default.local:7484/v1/alab";
    const PRIMARY: &str = "http://primary.local:7484/v1/alab";
    for (known_count, name_count) in [(0usize, 16usize), (2, 15), (16, 16), (35, 35)] {
        let known: Vec<_> = (0..known_count)
            .map(|n| format!("http://p{n}.local:7484/v1/alab"))
            .collect();
        let mut policy = known.clone();
        policy.extend([
            PRIMARY.to_owned(),
            DEFAULT.to_owned(),
            "not a URL".to_owned(),
            "http://foreign.local:7484/v1/elsewhere".to_owned(),
        ]);
        if known_count != 0 {
            policy.push("http://P0.LOCAL:7484/v1/alab#ignored-fragment".to_owned());
        }
        let names: Vec<_> = (0..name_count).map(|n| format!("n{n}")).collect();
        let expected_names: BTreeSet<_> = names.iter().cloned().collect();
        let expected: BTreeSet<_> = std::iter::once(DEFAULT.to_owned())
            .chain(known)
            .chain(
                names
                    .iter()
                    .map(|name| format!("http://{name}.local:7484/v1/alab")),
            )
            .collect();
        let defaults = [
            DEFAULT,
            PRIMARY,
            "http://DEFAULT.LOCAL:7484/v1/alab",
            "not a URL",
        ];
        let from = AtomicUsize::new(0);
        let mut queried = BTreeSet::new();
        let mut reached = BTreeSet::new();
        for _ in 0..(known_count + name_count).div_ceil(MAX_CANDIDATES - 1) {
            let plan = Plan::new(
                "alab",
                PRIMARY,
                &defaults,
                &policy,
                &[],
                Pool::Names(names.clone()),
                Cursor {
                    memory: &from,
                    saved: None,
                },
            );
            let urls = plan.finish("alab", PRIMARY, Duration::from_secs(1), |selected, _| {
                assert!(selected.len() < MAX_CANDIDATES);
                queried.extend(selected.iter().cloned());
                selected
                    .iter()
                    .map(|name| DiscoveredVault {
                        url: format!("http://{name}.local:7484/v1/alab"),
                        txt: None,
                    })
                    .collect()
            });
            assert!(urls.len() <= MAX_CANDIDATES);
            assert_eq!(urls.first().map(String::as_str), Some(DEFAULT));
            assert_eq!(urls.iter().collect::<BTreeSet<_>>().len(), urls.len());
            reached.extend(urls);
        }
        assert_eq!(
            queried, expected_names,
            "known={known_count}, names={name_count}"
        );
        assert_eq!(reached, expected, "known={known_count}, names={name_count}");
    }
}
