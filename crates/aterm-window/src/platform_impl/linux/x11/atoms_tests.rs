// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/x11/atoms.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use super::*;

#[test]
fn every_name_is_interned_in_index_order() {
    assert_eq!(NAMES.len(), _XSETTINGS_SETTINGS as usize + 1);
    assert_eq!(NAMES[CARD32 as usize], b"CARD32");
    assert_eq!(NAMES[TextUriList as usize], b"text/uri-list");
    assert_eq!(NAMES[Incr as usize], b"INCR");
    assert_eq!(NAMES[AtomName::None as usize], b"None");
    assert_eq!(NAMES[_XSETTINGS_SETTINGS as usize], b"_XSETTINGS_SETTINGS");
}
