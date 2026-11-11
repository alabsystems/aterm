// Copyright 2019 x11rb Contributers
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Derived from x11rb-protocol 0.13.2 src/resource_manager/ (MIT OR Apache-2.0, used under
// Apache-2.0), rewritten by the aterm project in 2026 as one module.

use super::*;

#[test]
fn xft_dpi_from_an_xrdb_property() {
    let db = Database::parse(b"Xft.antialias:\t1\nXft.dpi:\t144\n*background: #000\n");
    assert_eq!(db.get_string("Xft.dpi", ""), Some("144"));
    assert_eq!(db.get_string("Xft.hinting", ""), None);
    assert_eq!(
        db.get_string("aterm.background", "ATerm.Background"),
        Some("#000")
    );
    let escapes = Database::parse(b"aterm.one: \\1\naterm.two: \\12\naterm.eof: \\7");
    assert_eq!(escapes.get_string("aterm.one", ""), Some("\\1"));
    assert_eq!(escapes.get_string("aterm.two", ""), Some("\\12"));
    assert_eq!(escapes.get_string("aterm.eof", ""), Some("\\7"));
}

#[test]
fn a_hostile_entry_stays_bounded() {
    let mut line = Vec::new();
    for _ in 0..4096 {
        line.extend_from_slice(b"*a");
    }
    line.extend_from_slice(b": x\n");
    let db = Database::parse(&line);
    let query = vec!["a"; 64].join(".");
    let _ = db.get(&query, &query);
}
