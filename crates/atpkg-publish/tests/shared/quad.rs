// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The quad verifier is the CLIENT's chain: a quad the shipping signer makes verifies;
//! every way of breaking one is refused.

use std::sync::Arc;

use atpkg_publish::host::fake::FakeHost;
use atpkg_publish::quad::{self, QUAD_FILES, index_tag};
use atpkg_publish::testkit::Keys;

use super::scratch;

#[test]
fn a_quad_the_signer_made_verifies_and_reads_back_typed() {
    let tmp = scratch("quad-ok");
    let keys = Keys::mint("m3", &[]);
    let ctx = keys.ctx(tmp.path(), Arc::new(FakeHost::new()));
    let dir = tmp.path().join("quad");
    keys.write_quad(&dir, &keys.index_toml(41, &[("ay", "ay")], &[("ay", 18)]));
    let q = quad::verify_dir(&ctx, &dir).expect("the client chain accepts it");
    assert_eq!(q.index.index_build, 41);
    assert_eq!(q.index.attribution().machine_id, "m3");
    assert_eq!(q.roster.roster_seq, keys.roster.roster_seq);
}

#[test]
fn a_quad_missing_a_file_lapsed_or_signed_by_a_stranger_is_refused() {
    let tmp = scratch("quad-bad");
    let keys = Keys::mint("m3", &[]);
    let ctx = keys.ctx(tmp.path(), Arc::new(FakeHost::new()));

    let missing = tmp.path().join("missing");
    keys.write_quad(&missing, &keys.index_toml(41, &[], &[]));
    std::fs::remove_file(missing.join(QUAD_FILES[3])).expect("rm");
    let err = quad::verify_dir(&ctx, &missing).err().expect("refused");
    assert!(err.to_string().contains("aterm-machines.toml.sig"), "{err}");

    let lapsed = tmp.path().join("lapsed");
    let index = keys
        .index_toml(41, &[], &[])
        .replace("2099-01-01T00:00:00Z", "2020-01-01T00:00:00Z");
    keys.write_quad(&lapsed, &index);
    let err = quad::verify_dir(&ctx, &lapsed).err().expect("refused");
    assert!(err.to_string().contains("lapsed"), "{err}");

    let stranger = Keys::mint("m3", &[]);
    let foreign = tmp.path().join("foreign");
    keys.write_quad(&foreign, &keys.index_toml(41, &[], &[]));
    let forged = stranger.sign(&std::fs::read(foreign.join(QUAD_FILES[0])).expect("read"));
    std::fs::write(foreign.join(QUAD_FILES[1]), forged).expect("write");
    assert!(
        quad::verify_dir(&ctx, &foreign).is_err(),
        "an unrostered key's index"
    );

    let mut unarmed = ctx.clone();
    unarmed.anchor.clear();
    let ok = tmp.path().join("ok");
    keys.write_quad(&ok, &keys.index_toml(41, &[], &[]));
    let err = quad::verify_dir(&unarmed, &ok).err().expect("refused");
    assert!(err.to_string().contains("EMPTY"), "{err}");
}

#[test]
fn the_head_is_the_newest_published_index_and_a_transient_500_is_retried() {
    let tmp = scratch("quad-head");
    let keys = Keys::mint("m3", &[]);
    let host = Arc::new(FakeHost::new());
    let ctx = keys.ctx(tmp.path(), host.clone());
    let slug = ctx.index_slug();
    keys.publish_quad(&host, &slug, 34, &keys.index_toml(34, &[], &[]));
    keys.publish_quad(&host, &slug, 35, &keys.index_toml(35, &[], &[]));
    host.set_draft(&slug, &index_tag(35), true);
    host.fail_once(&slug, &index_tag(34), QUAD_FILES[3]);
    let (n, q) = quad::fetch_head(&ctx, &slug, &tmp.path().join("head")).expect("head");
    assert_eq!(n, 34, "a draft is a hole in the tag run, never the head");
    assert_eq!(q.index.index_build, 34);
}
