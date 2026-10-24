// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `status` over a fake channel.

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
    assert!(lines.iter().any(|l| l.contains("LAPSED:")), "{lines:?}");

    let dir = rig.path("quad");
    rig.quad_dir(&dir, &lapsed_doc());
    let on_disk = status::lines(&rig.ctx, Some(&dir)).expect("a lapsed quad on disk");
    assert!(on_disk.iter().any(|l| l.contains("LAPSED:")), "{on_disk:?}");

    let fresh = Rig::new("status-fresh");
    fresh.publish_head(&Doc::new(21, &[("ay", 5)]));
    let lines = status::lines(&fresh.ctx, None).expect("a fresh head");
    assert!(!lines.iter().any(|l| l.contains("LAPSED")), "{lines:?}");
}

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

#[test]
fn the_live_codex_row_is_said_carried_not_drift() {
    let rig = Rig::new("status-codex");
    let mut doc = Doc::new(71, &[("ay", 5), ("codex", 2_026_101_501)]);
    for p in &mut doc.programs {
        if p.0 == "codex" {
            p.1 = "aterm".into();
        }
    }
    rig.publish_head(&doc);
    let lines = status::lines(&rig.ctx, None).expect("the head");
    assert!(
        lines.iter().any(|l| l.starts_with(
            "[programs.codex] names repo \"aterm\", not ALab's \"codex-alab\": carried as it stands"
        )),
        "{lines:?}"
    );
    assert!(!lines.iter().any(|l| l.contains("drift")), "{lines:?}");

    let alab = Rig::new("status-codex-alab");
    alab.publish_head(&Doc::new(71, &[("ay", 5), ("codex", 2_026_101_601)]));
    let lines = status::lines(&alab.ctx, None).expect("the head");
    assert!(
        !lines.iter().any(|l| l.contains("[programs.codex]")),
        "{lines:?}"
    );
}
