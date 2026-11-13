// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::{ATTR_APPEND, ATTR_IMMUTABLE, binds, sticky_keeps};

/// AN IMMUTABLE OR APPEND-ONLY INODE KEEPS ITS NAME, as far as its filesystem reports the
/// attribute: either bit set and reported binds; a bit the mask does not report, or another
/// attribute (compressed 0x4, nodump 0x40), does not. NEGATIVE CONTROLS: the same bits with no
/// mask, and no bits with the whole mask.
#[test]
fn an_immutable_or_append_only_inode_keeps_its_name() {
    let all = u64::MAX;
    assert!(binds((ATTR_IMMUTABLE, all)));
    assert!(binds((ATTR_APPEND, all)));
    assert!(binds((ATTR_IMMUTABLE | 0x4, ATTR_IMMUTABLE)));
    assert!(!binds((ATTR_IMMUTABLE | ATTR_APPEND, 0)), "not reported");
    assert!(
        !binds((ATTR_IMMUTABLE, ATTR_APPEND)),
        "another bit reported"
    );
    assert!(!binds((0x4 | 0x40, all)), "other attributes");
    assert!(!binds((0, all)));
}

/// A STICKY FOLDER KEEPS ANOTHER'S NAME: in a sticky folder the caller may take an entry's
/// name only when the folder or the entry is theirs, or they are root. NEGATIVE CONTROL: the
/// same folder without the sticky bit keeps nothing.
#[test]
fn a_sticky_folder_keeps_the_name_of_an_entry_neither_it_nor_the_entry_is_the_callers() {
    let (me, them, other) = (501, 502, 503);
    assert!(sticky_keeps(0o1777, them, other, me));
    assert!(!sticky_keeps(0o1777, me, other, me), "the folder is mine");
    assert!(!sticky_keeps(0o1777, them, me, me), "the entry is mine");
    assert!(!sticky_keeps(0o1777, them, other, 0), "root");
    assert!(!sticky_keeps(0o0777, them, other, me), "not sticky");
}
