// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn key() -> StatKey {
    StatKey {
        dev: 1,
        ino: 2,
        mode: 0o100_644,
        size: 3,
        mtime_ns: 4,
        ctime_ns: 5,
    }
}

fn digest(walk: impl FnOnce(&mut StatDigest)) -> ([u8; 32], u64) {
    let mut digest = StatDigest::new(b"root");
    walk(&mut digest);
    digest.finish()
}

/// Every field of an entry's status, its path, a link's target and resolution, and a
/// directory's names each change the digest, and the framing keeps a boundary from moving
/// unseen. CONTROL: the same walk twice is one digest.
#[test]
fn each_field_path_target_and_name_changes_the_digest() {
    let base = |d: &mut StatDigest| {
        d.entry(b"", &key());
        d.names([&b"ab"[..]]);
        d.entry(b"ab", &key());
        d.link(b"target", Some(&key()));
    };
    let original = digest(base);
    assert_eq!(digest(base), original);
    assert_eq!(original.1, 2);
    let fields: [fn(&mut StatKey); 6] = [
        |k| k.dev += 1,
        |k| k.ino += 1,
        |k| k.mode ^= 0o100,
        |k| k.size += 1,
        |k| k.mtime_ns += 1,
        |k| k.ctime_ns += 1,
    ];
    for (nth, change) in fields.into_iter().enumerate() {
        let mut changed = key();
        change(&mut changed);
        assert_ne!(
            digest(|d| {
                d.entry(b"", &key());
                d.names([&b"ab"[..]]);
                d.entry(b"ab", &changed);
                d.link(b"target", Some(&key()));
            }),
            original,
            "entry field {nth}"
        );
        assert_ne!(
            digest(|d| {
                d.entry(b"", &key());
                d.names([&b"ab"[..]]);
                d.entry(b"ab", &key());
                d.link(b"target", Some(&changed));
            }),
            original,
            "resolved field {nth}"
        );
    }
    let variants: [fn(&mut StatDigest); 5] = [
        |d| {
            d.entry(b"", &key());
            d.names([&b"a"[..], &b"b"[..]]);
            d.entry(b"ab", &key());
            d.link(b"target", Some(&key()));
        },
        |d| {
            d.entry(b"", &key());
            d.names([&b"ab"[..]]);
            d.entry(b"ac", &key());
            d.link(b"target", Some(&key()));
        },
        |d| {
            d.entry(b"", &key());
            d.names([&b"ab"[..]]);
            d.entry(b"ab", &key());
            d.link(b"other", Some(&key()));
        },
        |d| {
            d.entry(b"", &key());
            d.names([&b"ab"[..]]);
            d.entry(b"ab", &key());
            d.link(b"target", None);
        },
        |d| {
            d.entry(b"", &key());
            d.names([&b"ab"[..]]);
            d.entry(b"ab", &key());
            d.link(b"target", Some(&key()));
            d.ancestor();
        },
    ];
    for (nth, variant) in variants.into_iter().enumerate() {
        assert_ne!(digest(variant), original, "variant {nth}");
    }
    let mut other = StatDigest::new(b"other root");
    base(&mut other);
    assert_ne!(other.finish(), original, "the label");
}

/// The newest status change is the latest of every status recorded, an entry's own or a
/// link's resolved one, and none before any. CONTROL: a walk of older statuses only.
#[test]
fn the_newest_change_is_the_latest_status_any_record_holds() {
    let at = |ctime_ns| StatKey { ctime_ns, ..key() };
    let mut d = StatDigest::new(b"root");
    assert_eq!(d.newest_change_ns(), None);
    d.entry(b"", &at(7));
    d.entry(b"a", &at(3));
    assert_eq!(d.newest_change_ns(), Some(7));
    d.entry(b"link", &at(5));
    d.link(b"far", Some(&at(9)));
    d.link(b"gone", None);
    assert_eq!(d.newest_change_ns(), Some(9), "the resolved status counts");
}

/// A write whose modification time is put back still moves the status-change time, which
/// the read's hold and the digest both see: APFS stamps it in nanoseconds (a jiffy clock,
/// Linux's before 6.13, can stamp the write with the time it replaced, which is what
/// [`SETTLE_NS`] is for). CONTROL: the status read twice with nothing between is unchanged.
#[cfg(target_os = "macos")]
#[test]
fn a_write_with_its_modification_time_put_back_still_changes_the_status() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("compiler");
    std::fs::write(&path, b"one").unwrap();
    let before = StatKey::of(&std::fs::metadata(&path).unwrap());
    assert!(before.unchanged(&StatKey::of(&std::fs::metadata(&path).unwrap())));
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    std::fs::write(&path, b"two").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(modified))
        .unwrap();
    let after = StatKey::of(&std::fs::metadata(&path).unwrap());
    assert_eq!(
        (after.size, after.mtime_ns),
        (before.size, before.mtime_ns),
        "only the status-change time tells"
    );
    assert!(!before.unchanged(&after));
    assert_ne!(
        digest(|d| d.entry(b"compiler", &before)),
        digest(|d| d.entry(b"compiler", &after))
    );
}
