// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn nested_roots_keep_a_witness_each_consumer_can_place() {
    let dirs = [PathBuf::from("/cache"), PathBuf::from("/cache/sub\nline")];
    let row = b"1000-2000 r--p 0000 01:02 7 /cache/sub\\012line/file\n";
    let got = parse(row, 7, &dirs).unwrap().expect("mapped file");
    assert!(
        dirs.iter()
            .all(|root| placed(&got.path, std::slice::from_ref(root)).is_some()),
        "{got:?}"
    );
}

#[test]
fn ambiguous_known_roots_are_unknown_instead_of_selecting_one() {
    let dirs = [
        PathBuf::from("/cache\\012line"),
        PathBuf::from("/cache\nline"),
    ];
    let row = b"1000-2000 r--p 0000 01:02 7 /cache\\012line/file\n";
    assert!(
        parse(row, 7, &dirs).is_err(),
        "neither possible owner may be ruled out"
    );
    for root in &dirs {
        let got = parse(row, 7, std::slice::from_ref(root))
            .unwrap()
            .expect("possible holder");
        assert!(placed(&got.path, std::slice::from_ref(root)).is_some());
    }
}

#[test]
fn maps_keep_raw_paths_spaces_newlines_and_deleted_names() {
    for name in [
        b"/cache/input with spaces".as_slice(),
        b"/cache/byte-\xff",
        b"/cache/input (deleted)",
    ] {
        let mut row = b"1000-2000 r--p 0000 01:02 7 ".to_vec();
        row.extend_from_slice(name);
        row.push(b'\n');
        let got = parse(&row, 7, &[PathBuf::from("/cache")])
            .expect("complete row")
            .expect("mapped file");
        assert_eq!(got.how, "map");
        assert_eq!(got.path.as_os_str().as_bytes(), name);
    }
    let row = b"1000-2000 r--p 0000 01:02 7 /cache\\012line/file\n";
    for root in ["/cache\nline", "/cache\\012line"] {
        assert!(
            parse(row, 7, &[PathBuf::from(root)]).unwrap().is_some(),
            "{root:?}"
        );
    }
    let root = PathBuf::from("/cache\nline\\012literal");
    for prefix in ["", "/System/Volumes/Data"] {
        let row = format!("1000-2000 r--p 0000 01:02 7 {prefix}/cache\\012line\\012literal/file\n");
        let got = parse(row.as_bytes(), 7, std::slice::from_ref(&root))
            .unwrap()
            .expect("mixed newline and literal escape preserve a possible holder");
        assert!(
            got.path.starts_with(&root),
            "the consumer can place the witness: {got:?}"
        );
    }
}

#[test]
fn incomplete_malformed_and_unreadable_maps_are_unknown() {
    for row in [
        b"1000-2000 r--p 0000 01:02 7 /other\ntruncated".as_slice(),
        b"1000-2000 r--p 0000 01:02 7 /other",
        b"1000-2000 r--p 0000 01:02 7 /other\0name\n",
        b"1000-2000 r--p 0000 01:02 7\n",
        b"2000-1000 r--p 0000 01:02 7 /other\n",
        b"1000-2000 ---- 0000 01:02 0\n",
        b"+1000-2000 r--p 0000 01:02 7 /other\n",
        b"1000-2000 r--p 0000 01:+02 7 /other\n",
        b"1000-2000 r--p 0000 01:02 +7 /other\n",
        b"1000-2000 r--p 0000 01:02 0\n1000-2000 r--p 0000 01:02 0\n",
    ] {
        assert!(
            parse(row, 7, &[PathBuf::from("/cache")]).is_err(),
            "{row:?}"
        );
    }
    let mut oversized = b"1000-2000 r--p 0000 01:02 7 /other/".to_vec();
    oversized.extend(std::iter::repeat_n(b'x', MAX_BYTES));
    oversized.push(b'\n');
    assert!(
        parse(&oversized, 7, &[]).is_err(),
        "otherwise valid data still has a bound"
    );
    let world = aterm_tempfile::tempdir().unwrap();
    assert!(
        uses(world.path(), 7, &[]).is_err(),
        "missing maps of an existing process is unknown"
    );
    std::fs::create_dir(world.path().join("maps")).unwrap();
    assert!(
        uses(world.path(), 7, &[]).is_err(),
        "a failed read is unknown"
    );
    assert!(
        uses(&world.path().join("exited"), 7, &[])
            .unwrap()
            .is_none()
    );
    assert!(
        parse(
            b"1000-2000 rw-p 0000 00:00 0 [heap]\n2000-3000 r--p 0000 01:02 1 /cache-neighbor\n",
            7,
            &[PathBuf::from("/cache")]
        )
        .unwrap()
        .is_none()
    );
}
