// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn fixture() -> String {
    let path = aterm_tempfile::manifest_dir().join("tests/fixtures/site/public/sitemap.xml");
    std::fs::read_to_string(path).unwrap()
}

#[test]
fn releases_is_added_once_and_touched_pages_move_to_today() {
    let up = update(
        &fixture(),
        "/releases",
        "2026-08-29",
        &["/aterm", "/terminal"],
    )
    .unwrap();
    assert!(up.added);
    assert_eq!(
        lastmods(&up.text),
        [
            ("https://alab.systems/".into(), "2026-08-24".into()),
            ("https://alab.systems/aterm".into(), "2026-08-29".into()),
            ("https://alab.systems/terminal".into(), "2026-08-29".into()),
            ("https://alab.systems/mission".into(), "2026-08-27".into()),
            ("https://alab.systems/releases".into(), "2026-08-29".into()),
        ]
    );
    let again = update(&up.text, "/releases", "2026-08-29", &[]).unwrap();
    assert_eq!((again.text, again.added), (up.text, false));
}

#[test]
fn a_sitemap_out_of_shape_or_a_bad_date_is_refused() {
    assert!(update("<urlset></urlset>", "/releases", "2026-08-29", &[]).is_err());
    assert!(update(&fixture(), "/releases", "29-08-2026", &[]).is_err());
}
