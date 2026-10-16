// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `status` over a fake channel: a head past its horizon is still described — verified
//! but for that horizon, and marked LAPSED — while a quad the client chain does not
//! accept is refused as ever.

use atpkg_publish::channel::status;
use atpkg_publish::testkit::Keys;

use super::{Doc, Rig};

const LAPSED: &str = "2026-01-01T00:00:00Z";

fn lapsed_doc() -> Doc {
    let mut doc = Doc::new(21, &[("ay", 5)]);
    doc.valid_until = LAPSED.to_string();
    doc
}

#[test]
fn a_lapsed_head_is_described_and_marked_lapsed() {
    let rig = Rig::new("status-lapsed");
    rig.publish_head(&lapsed_doc());
    let lines = status::lines(&rig.ctx, None).expect("a lapsed head is still described");
    assert!(
        lines[0].starts_with("atpkg-index-21 — signed by machine m3"),
        "{lines:?}"
    );
    assert!(
        lines[1].starts_with(&format!("horizon: index {LAPSED}")) && lines[1].contains("LAPSED:"),
        "{lines:?}"
    );
    assert!(lines.iter().any(|l| l == "  pin ay = 5"), "{lines:?}");

    let dir = rig.path("quad");
    rig.quad_dir(&dir, &lapsed_doc());
    let on_disk = status::lines(&rig.ctx, Some(&dir)).expect("a lapsed quad on disk");
    assert!(on_disk[1].contains("LAPSED:"), "{on_disk:?}");

    let fresh = Rig::new("status-fresh");
    fresh.publish_head(&Doc::new(21, &[("ay", 5)]));
    let lines = status::lines(&fresh.ctx, None).expect("a fresh head");
    assert!(!lines.iter().any(|l| l.contains("LAPSED")), "{lines:?}");
}

/// Setting the horizon aside sets nothing else aside: a quad signed under another master
/// is refused, lapsed or not.
#[test]
fn a_foreign_quad_is_refused_whatever_its_horizon() {
    let rig = Rig::new("status-foreign");
    let other = Keys::mint("m9", &[]);
    for doc in [lapsed_doc(), Doc::new(21, &[("ay", 5)])] {
        let dir = rig.path(&format!("foreign-{}", doc.valid_until.replace(':', "")));
        other.write_quad(&dir, &doc.render(&other));
        assert!(
            status::lines(&rig.ctx, Some(&dir)).is_err(),
            "{}",
            doc.valid_until
        );
    }
}
