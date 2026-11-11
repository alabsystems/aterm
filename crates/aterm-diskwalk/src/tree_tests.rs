// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn grouping_reads_like_the_design() {
    assert_eq!(group(104_211), "104,211");
    assert_eq!(group(100_000), "100,000");
    assert_eq!(group(999), "999");
    assert_eq!(group(1_000), "1,000");
}

#[test]
fn biggest_follows_a_chain_down_to_where_the_space_spreads() {
    let mut t = Tree::new(PathBuf::from("/r"), 1);
    let a = t.push_child(DirId::ROOT, b"a", 2);
    let deep = t.push_child(a, b"deep", 3);
    let x = t.push_child(deep, b"x", 4);
    let y = t.push_child(deep, b"y", 5);
    let b = t.push_child(DirId::ROOT, b"b", 6);
    for (id, bytes) in [(x, 45), (y, 40), (deep, 100), (a, 100), (b, 30)] {
        t.rec_mut(id).subtree.alloc_fair = bytes;
    }
    assert_eq!(t.biggest(3), vec![deep, x, y]);
    t.rec_mut(x).subtree.alloc_fair = 60;
    t.rec_mut(y).subtree.alloc_fair = 25;
    assert_eq!(t.biggest(3), vec![x, b, y]);
    assert_eq!(t.rel_path(x), PathBuf::from("a/deep/x"));
    assert_eq!(t.children(DirId::ROOT), vec![a, b]);
}

#[test]
fn a_folder_is_found_by_name_and_queries_scope_to_it() {
    let mut t = Tree::new(PathBuf::from("/r"), 1);
    let a = t.push_child(DirId::ROOT, b"a", 2);
    let deep = t.push_child(a, b"deep", 3);
    let x = t.push_child(deep, b"x", 4);
    let y = t.push_child(deep, b"y", 5);
    let b = t.push_child(DirId::ROOT, b"b", 6);
    for (id, bytes, entries) in [
        (x, 45, 9),
        (y, 40, 3),
        (deep, 100, 2),
        (a, 100, 1),
        (b, 30, 80),
    ] {
        let r = t.rec_mut(id);
        r.subtree.alloc_fair = bytes;
        r.direct_entries = entries;
    }
    t.rec_mut(b).refused = Some(Refusal::Protected);
    assert_eq!(t.find(Path::new("")), Some(DirId::ROOT));
    assert_eq!(t.find(Path::new("a/deep")), Some(deep));
    assert_eq!(t.find(Path::new("a/./deep/y")), Some(y));
    assert_eq!(t.find(Path::new("a/none")), None);
    assert_eq!(t.find(Path::new("../a")), None);
    assert_eq!(t.find(Path::new("A/Deep")), None, "names match exactly");
    assert_eq!(t.find_folded(Path::new("A/Deep"), true), Some(deep));
    assert_eq!(t.find_folded(Path::new("A/Deep"), false), None);
    assert!(t.is_within(x, a) && t.is_within(a, a) && !t.is_within(b, a));
    assert_eq!(t.biggest_in(deep, 5), vec![x, y]);
    assert_eq!(t.most_entries_in(a, 2), vec![x, y]);
    assert_eq!(t.most_entries(1), vec![b]);
    assert_eq!(t.direct_entries_max_in(a), 9);
    assert!(t.refused_in(a).is_empty());
    assert_eq!(t.refused(), vec![(b, Refusal::Protected)]);
}

#[test]
fn a_huge_directory_is_a_note_then_a_warning() {
    let mut t = Tree::new(PathBuf::from("/w"), 1);
    let deps = t.push_child(DirId::ROOT, b"deps", 2);
    t.rec_mut(deps).direct_entries = 104_211;
    t.rec_mut(deps).direct_exec = 3;
    let tmp = t.push_child(DirId::ROOT, b"tmp", 3);
    t.rec_mut(tmp).direct_entries = 80_000;
    let huge = t.huge_dirs();
    assert_eq!(huge.len(), 2);
    assert!(huge[0].warning && huge[0].executables);
    assert!(huge[0].sentence().starts_with(&format!(
        "warning: {} has 104,211 entries (line 100,000): a binary launched from it",
        Path::new("/w").join("deps").display()
    )));
    assert!(!huge[1].warning);
    assert!(
        huge[1]
            .sentence()
            .contains("huge directory: slow for anything that lists it")
    );
}

/// Out-of-bounds or wrapping `(name_at, name_len)` spans return an empty slice without panicking.
/// NEGATIVE CONTROL: unchecked `at + len` overflows on 32-bit targets when `name_at == u32::MAX`.
#[test]
fn name_handles_wrapping_offsets_without_panicking() {
    let mut t = Tree::new(PathBuf::from("/w"), 1);
    let child = t.push_child(DirId::ROOT, b"ok", 2);
    assert_eq!(t.name(child), b"ok");
    t.rec_mut(child).name_at = u32::MAX;
    t.rec_mut(child).name_len = u32::MAX;
    assert_eq!(t.name(child), b"");
}
