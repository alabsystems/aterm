// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The reach rule's premise on the world (P3's `OneIsFixed`): a URL that may move between
/// servers never reads as one server. Each of the ALab vault's compiled-in names moves
/// between the Macs that serve it, so none settles a hold or a pending set by reach.
#[test]
fn no_alab_name_reads_as_one_server() {
    assert!(!crate::ALAB_DEFAULT_URLS.is_empty());
    for url in crate::ALAB_DEFAULT_URLS {
        assert!(!names_one_server(url), "{url} reads as one server");
    }
    assert!(names_one_server("http://192.0.2.7:7485/v/one"));
}
