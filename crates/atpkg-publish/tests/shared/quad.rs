// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The quad verifier is the client's chain: a quad the shipping signer makes verifies;
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

/// A KEYLESS BUILDER READS THE HEAD ANONYMOUSLY (D17): with no token file, every
/// authenticated call fails, so the head is walked and fetched on the
/// download host as a client does, and verified by the client chain as before — the
/// newest published build, a draft above it a hole, a forged quad still refused.
#[test]
fn a_keyless_builder_reads_the_head_where_a_client_does() {
    let tmp = scratch("quad-anon");
    let keys = Keys::mint("m3", &[]);
    let host = Arc::new(FakeHost::new());
    let ctx = keys.ctx(tmp.path(), host.clone());
    let slug = ctx.index_slug();
    for n in 1..=5 {
        keys.publish_quad(&host, &slug, n, &keys.index_toml(n, &[], &[]));
    }
    host.set_draft(&slug, &index_tag(5), true);
    host.set_anonymous_reads(true);
    let (n, q) = quad::fetch_head(&ctx, &slug, &tmp.path().join("head")).expect("head");
    assert_eq!(
        n, 4,
        "a draft is no publication on the download host either"
    );
    assert_eq!(q.index.index_build, 4);
    let calls = host.calls();
    assert!(
        calls
            .iter()
            .all(|c| c.starts_with("probe ") || c.starts_with("fetch ")),
        "only the anonymous download host is asked: {calls:?}"
    );
    // The roster the signer elects comes the same way.
    let authority = atpkg_publish::artifact::sign::authority(&ctx).expect("elected");
    assert_eq!(authority.seq(), keys.roster.roster_seq);
    assert!(
        authority.source.contains("the head of"),
        "{}",
        authority.source
    );

    // An empty channel serves no index; a forged head is refused by the client chain.
    let empty = Arc::new(FakeHost::new());
    empty.set_anonymous_reads(true);
    let mut bare = ctx.clone();
    bare.hosts = atpkg_publish::host::Hosts::single(empty);
    let Err(err) = quad::fetch_head(&bare, &slug, &tmp.path().join("none")) else {
        panic!("an empty channel has no head");
    };
    assert!(err.to_string().contains("serves no published"), "{err}");
    let stranger = Keys::mint("m3", &[]);
    host.set_draft(&slug, &index_tag(5), false);
    host.put_asset(
        &slug,
        &index_tag(5),
        QUAD_FILES[1],
        stranger.sign(keys.index_toml(5, &[], &[]).as_bytes()),
    );
    assert!(
        quad::fetch_head(&ctx, &slug, &tmp.path().join("forged")).is_err(),
        "a head the client chain refuses"
    );
}
