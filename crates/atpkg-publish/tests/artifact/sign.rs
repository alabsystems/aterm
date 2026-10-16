// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The signing ceremony: a staged manifest is re-checked, signed with this machine's key
//! and the pair verified under the newest master-verified roster before its signature is
//! written; a refused manifest gets no signature and keeps the one it had; an `unproven`
//! row is signed only while the receipt switch reads dormant.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use atpkg_keys::roster_ops::{add, revoke};
use atpkg_publish::artifact::pkgdoc::{self, Kind, Row};
use atpkg_publish::artifact::sign;
use atpkg_publish::ctx::Ctx;
use atpkg_publish::error::Error;
use atpkg_publish::host::Slug;
use atpkg_publish::host::fake::FakeHost;
use atpkg_publish::quad::{QUAD_FILES, index_tag};
use atpkg_publish::testkit::{Keys, NOW};
use atpkg_publish::triple::AARCH64_APPLE_DARWIN;

use super::fixture::{DORMANT_LEDGER, MINTED_LEDGER, client_verifies, put, scratch, trust_repo};
use super::pkgdoc::{header, provenance, row};

/// A channel whose head (index 41) this machine signed, and its context.
fn channel(keys: &Keys, home: &Path) -> (Ctx, Arc<FakeHost>) {
    let host = Arc::new(FakeHost::new());
    let ctx = keys.ctx(home, host.clone());
    keys.publish_quad(
        &host,
        &ctx.index_slug(),
        41,
        &keys.index_toml(41, &[("ay", "ay")], &[("ay", 9)]),
    );
    (ctx, host)
}

fn staged(dir: &Path, r: &Row) -> PathBuf {
    let pkg = dir.join(pkgdoc::file_name("ay", 9));
    put(&pkg, pkgdoc::fresh(&header(), r).expect("render").bytes());
    pkg
}

/// A master-signed roster pair: `(bytes, sig)`.
fn pair(keys: &Keys, roster: &aterm_update_core::Roster) -> (Vec<u8>, Vec<u8>) {
    let bytes = roster.to_toml().expect("the roster emits").into_bytes();
    let sig = keys.master_sign(&bytes);
    (bytes, sig)
}

/// Install a roster pair where this machine keeps its own (`~/.aterm/roster/`).
fn install_roster(ctx: &Ctx, (bytes, sig): &(Vec<u8>, Vec<u8>)) {
    put(&ctx.identity.roster, bytes);
    put(&sign::sig_path(&ctx.identity.roster), sig);
}

/// Publish index `n` on the channel carrying this roster pair (the index's own bytes
/// are no part of who may sign a pack).
fn publish_roster(host: &FakeHost, slug: &Slug, n: u64, (bytes, sig): &(Vec<u8>, Vec<u8>)) {
    let tag = index_tag(n);
    host.put_asset(slug, &tag, QUAD_FILES[0], b"index".to_vec());
    host.put_asset(slug, &tag, QUAD_FILES[1], b"sig".to_vec());
    host.put_asset(slug, &tag, QUAD_FILES[2], bytes.clone());
    host.put_asset(slug, &tag, QUAD_FILES[3], sig.clone());
}

fn now() -> u64 {
    u64::try_from(NOW).expect("a positive clock")
}

#[test]
fn a_staged_manifest_is_signed_and_verified_under_the_channel_head() {
    let tmp = scratch("sign-ok");
    let keys = Keys::mint("m3", &[]);
    let (ctx, _) = channel(&keys, tmp.path());
    let pkg = staged(tmp.path(), &row(AARCH64_APPLE_DARWIN, Kind::Binary));
    let sigs = sign::sign_all(&ctx, std::slice::from_ref(&pkg)).expect("signed");
    assert_eq!(sigs, vec![sign::sig_path(&pkg)]);
    client_verifies(&ctx, &pkg, &sigs[0])
        .expect("the pair the ceremony left is one the client accepts");
}

#[test]
fn a_key_no_roster_lists_writes_no_signature_and_leaves_an_existing_one_alone() {
    let tmp = scratch("sign-stranger");
    let keys = Keys::mint("m3", &[]);
    let (ctx, _) = channel(&keys, tmp.path());
    let stranger = Keys::mint("m4", &[]);
    std::fs::write(&ctx.identity.key, &stranger.machine_key).expect("swap the key");
    let pkg = staged(tmp.path(), &row(AARCH64_APPLE_DARWIN, Kind::Binary));
    let theirs = b"a rostered machine's signature over these bytes".to_vec();
    put(&sign::sig_path(&pkg), &theirs);
    let err = sign::sign_all(&ctx, std::slice::from_ref(&pkg)).expect_err("not rostered");
    assert!(matches!(err, Error::Refused(_)), "{err}");
    assert_eq!(
        std::fs::read(sign::sig_path(&pkg)).expect("still there"),
        theirs,
        "a ceremony run on an unrostered box never destroys another machine's signature"
    );
}

#[test]
fn an_unproven_row_is_signed_only_while_the_switch_reads_dormant() {
    let tmp = scratch("sign-unproven");
    let keys = Keys::mint("m3", &[]);
    let (mut ctx, _) = channel(&keys, tmp.path());
    let mut r = row(AARCH64_APPLE_DARWIN, Kind::Binary);
    r.provenance = provenance("unproven");
    let pkg = staged(tmp.path(), &r);

    ctx.trust_repo = tmp.path().join("trust-live");
    trust_repo(&ctx.trust_repo, MINTED_LEDGER);
    let err = sign::sign_all(&ctx, std::slice::from_ref(&pkg)).expect_err("live switch");
    let text = err.to_string();
    assert!(
        text.contains("lineage=unproven") && text.contains("receipt-carrying compiler"),
        "{text}"
    );
    assert!(!sign::sig_path(&pkg).exists());

    ctx.trust_repo = tmp.path().join("no-checkout");
    let err = sign::sign_all(&ctx, std::slice::from_ref(&pkg)).expect_err("unknown switch");
    assert!(
        !err.to_string().contains("receipt-carrying"),
        "a switch nobody could read is no reason for another compiler: {err}"
    );

    ctx.trust_repo = tmp.path().join("trust-dormant");
    trust_repo(&ctx.trust_repo, DORMANT_LEDGER);
    sign::sign_all(&ctx, std::slice::from_ref(&pkg)).expect("dormant: signed");
    let parsed = client_verifies(&ctx, &pkg, &sign::sig_path(&pkg)).expect("verifies");
    assert!(sign::has_unproven_row(&parsed));
}

#[test]
fn the_ceremony_re_checks_every_row_and_the_version_before_it_signs() {
    let tmp = scratch("sign-recheck");
    let keys = Keys::mint("m3", &[]);
    let (ctx, _) = channel(&keys, tmp.path());
    let good = String::from_utf8(
        pkgdoc::fresh(&header(), &row(AARCH64_APPLE_DARWIN, Kind::Binary))
            .expect("render")
            .bytes()
            .to_vec(),
    )
    .expect("utf8");
    let commit = &provenance("seed-mint").compiler_commit;
    let no_provenance = good[..good.find("[artifact.provenance]").expect("a record")].to_string();
    for (what, text, want) in [
        (
            "findings",
            good.replace("scan_findings = \"0\"", "scan_findings = \"3\""),
            "scan finding",
        ),
        ("commit", good.replace(commit.as_str(), "abc"), "40 hex"),
        (
            "no provenance",
            no_provenance,
            "carries no [artifact.provenance]",
        ),
        (
            "dirty",
            good.replace("version = \"0.2.0\"", "version = \"0.2.0-dirty\""),
            "never signed",
        ),
    ] {
        let pkg = tmp.path().join(what).join(pkgdoc::file_name("ay", 9));
        put(&pkg, text.as_bytes());
        let err = sign::sign_all(&ctx, std::slice::from_ref(&pkg)).expect_err(what);
        assert!(err.to_string().contains(want), "{what}: {err}");
        assert!(!sign::sig_path(&pkg).exists(), "{what}: nothing signed");
    }
}

#[test]
fn a_first_publish_signs_under_the_roster_this_machine_installed() {
    let tmp = scratch("sign-first");
    let keys = Keys::mint("m3", &[]);
    let ctx = keys.ctx(tmp.path(), Arc::new(FakeHost::new()));
    let pkg = staged(tmp.path(), &row(AARCH64_APPLE_DARWIN, Kind::Binary));
    let err = sign::sign_all(&ctx, std::slice::from_ref(&pkg)).expect_err("no roster at all");
    assert!(
        err.to_string().contains("no master-signed roster pair"),
        "{err}"
    );

    install_roster(&ctx, &(keys.roster_bytes.clone(), keys.roster_sig.clone()));
    sign::sign_all(&ctx, std::slice::from_ref(&pkg)).expect("signed offline, no index yet");
    let authority = sign::authority(&ctx).expect("elected");
    authority
        .verify_pkg(
            &std::fs::read(&pkg).expect("pkg"),
            &std::fs::read(sign::sig_path(&pkg)).expect("sig"),
        )
        .expect("verifies under the installed roster");
}

#[test]
fn a_machine_that_joined_since_the_head_signs_under_the_newer_roster() {
    let tmp = scratch("sign-joined");
    let keys = Keys::mint("m3", &[]);
    let (ctx, _) = channel(&keys, tmp.path());
    let (m4_key, m4_pub) = atpkg_keys::generate().expect("keypair");
    let joined = add(keys.roster.clone(), "m4", &m4_pub, now()).expect("roster m4");
    install_roster(&ctx, &pair(&keys, &joined));
    std::fs::write(&ctx.identity.key, &m4_key).expect("this box is m4");
    let pkg = staged(tmp.path(), &row(AARCH64_APPLE_DARWIN, Kind::Binary));
    sign::sign_all(&ctx, std::slice::from_ref(&pkg)).expect("signed before any index lists m4");
    let authority = sign::authority(&ctx).expect("elected");
    assert_eq!(authority.seq(), joined.roster_seq);
    assert!(authority.seq() > keys.roster.roster_seq);
}

#[test]
fn a_revocation_the_head_carries_outranks_a_stale_local_roster() {
    let tmp = scratch("sign-revoked");
    let keys = Keys::mint("m3", &[]);
    let host = Arc::new(FakeHost::new());
    let ctx = keys.ctx(tmp.path(), host.clone());
    install_roster(&ctx, &(keys.roster_bytes.clone(), keys.roster_sig.clone()));
    let revoked = revoke(keys.roster.clone(), "m3", now()).expect("revoke m3");
    publish_roster(&host, &ctx.index_slug(), 42, &pair(&keys, &revoked));
    let pkg = staged(tmp.path(), &row(AARCH64_APPLE_DARWIN, Kind::Binary));
    let err = sign::sign_all(&ctx, std::slice::from_ref(&pkg)).expect_err("revoked");
    assert!(matches!(err, Error::Refused(_)), "{err}");
    assert!(!sign::sig_path(&pkg).exists());
}

#[test]
fn two_rosters_at_one_generation_are_a_fork_and_nothing_is_signed() {
    let tmp = scratch("sign-fork");
    let keys = Keys::mint("m3", &[]);
    let host = Arc::new(FakeHost::new());
    let ctx = keys.ctx(tmp.path(), host.clone());
    let (_, m4_pub) = atpkg_keys::generate().expect("keypair");
    let (_, m5_pub) = atpkg_keys::generate().expect("keypair");
    let ours = add(keys.roster.clone(), "m4", &m4_pub, now()).expect("m4");
    let theirs = add(keys.roster.clone(), "m5", &m5_pub, now()).expect("m5");
    assert_eq!(ours.roster_seq, theirs.roster_seq);
    install_roster(&ctx, &pair(&keys, &ours));
    publish_roster(&host, &ctx.index_slug(), 42, &pair(&keys, &theirs));
    let pkg = staged(tmp.path(), &row(AARCH64_APPLE_DARWIN, Kind::Binary));
    let err = sign::sign_all(&ctx, std::slice::from_ref(&pkg)).expect_err("fork");
    assert!(err.to_string().contains("lineage fork"), "{err}");
    assert!(!sign::sig_path(&pkg).exists());
}

#[test]
fn the_verb_names_its_operands_and_its_flags() {
    let tmp = scratch("sign-verb");
    let keys = Keys::mint("m3", &[]);
    let (ctx, _) = channel(&keys, tmp.path());
    assert!(matches!(sign::run(&ctx, &[]), Err(Error::Usage(_))));
    assert!(matches!(
        sign::run(&ctx, &["--bogus".into()]),
        Err(Error::Usage(_))
    ));
    let pkg = staged(tmp.path(), &row(AARCH64_APPLE_DARWIN, Kind::Binary));
    let shown = pkg.to_string_lossy().into_owned();
    assert!(matches!(
        sign::run(&ctx, &[shown.clone(), "--trust-repo".into()]),
        Err(Error::Usage(_))
    ));
    sign::run(&ctx, &[shown]).expect("signed through the verb");
    client_verifies(&ctx, &pkg, &sign::sig_path(&pkg)).expect("verifies");
}

#[test]
fn the_verb_reads_the_switch_from_the_trust_repo_it_names() {
    let tmp = scratch("sign-verb-trust");
    let keys = Keys::mint("m3", &[]);
    let (ctx, _) = channel(&keys, tmp.path());
    let mut r = row(AARCH64_APPLE_DARWIN, Kind::Binary);
    r.provenance = provenance("unproven");
    let pkg = staged(tmp.path(), &r);
    let shown = pkg.to_string_lossy().into_owned();
    assert!(
        sign::run(&ctx, std::slice::from_ref(&shown)).is_err(),
        "no $HOME/trust: nothing shows the switch is dormant"
    );
    let elsewhere = tmp.path().join("src/trust");
    trust_repo(&elsewhere, DORMANT_LEDGER);
    sign::run(
        &ctx,
        &[
            "--trust-repo".into(),
            elsewhere.to_string_lossy().into_owned(),
            shown,
        ],
    )
    .expect("dormant where the verb was pointed");
}
