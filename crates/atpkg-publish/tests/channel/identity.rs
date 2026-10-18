// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The index signer's identity checks (`identity::signer_for`): each common mistake is
//! refused before anything is signed, naming the fix that works — the self-check's own
//! refusal is deliberately opaque (§8: no verification oracle).

use std::sync::Arc;

use atpkg_publish::host::fake::FakeHost;
use atpkg_publish::identity::signer_for;
use atpkg_publish::testkit::Keys;

use super::scratch;

#[test]
fn a_rostered_machine_signs_as_itself() {
    let tmp = scratch("id-ok");
    let keys = Keys::mint("m3", &[]);
    let ctx = keys.ctx(tmp.path(), Arc::new(FakeHost::new()));
    let s = signer_for(&ctx, &keys.roster).expect("listed");
    assert_eq!(s.record.id, "m3");
    assert_eq!(s.roster_seq, keys.roster.roster_seq);
    assert_eq!(s.key.pubkey_b64().expect("pub"), keys.machine_pub);
}

/// A listed entry past its `not_after` signs what every client refuses: refused, and
/// revocation still outranks it (a producer bug that leaves an id both listed and
/// revoked is read as revoked).
#[test]
fn an_expired_or_revoked_entry_is_refused_even_while_listed() {
    let tmp = scratch("id-expired");
    let mut keys = Keys::mint("m3", &[]);
    keys.roster
        .machines
        .iter_mut()
        .filter(|m| m.id == "m3")
        .for_each(|m| m.not_after = Some("2026-01-01T00:00:00Z".into()));
    let ctx = keys.ctx(tmp.path(), Arc::new(FakeHost::new()));
    let err = signer_for(&ctx, &keys.roster)
        .expect_err("expired")
        .to_string();
    assert!(err.contains("Expired"), "{err}");
    keys.roster.revoked.push("m3".into());
    let err = signer_for(&ctx, &keys.roster)
        .expect_err("revoked")
        .to_string();
    assert!(err.contains("machine 'm3' is REVOKED"), "{err}");
}

#[test]
fn a_record_that_relabels_the_key_or_names_another_key_is_refused() {
    let tmp = scratch("id-relabel");
    let keys = Keys::mint("m3", &[]);
    let ctx = keys.ctx(tmp.path(), Arc::new(FakeHost::new()));
    std::fs::write(
        &ctx.identity.record,
        format!("id = \"m11\"\npubkey = \"{}\"\n", keys.machine_pub),
    )
    .expect("record");
    let err = signer_for(&ctx, &keys.roster)
        .expect_err("relabel")
        .to_string();
    assert!(
        err.contains("lists this machine's key under id 'm3'"),
        "{err}"
    );

    let (_, other_pub) = atpkg_keys::generate().expect("keypair");
    std::fs::write(
        &ctx.identity.record,
        format!("id = \"m3\"\npubkey = \"{other_pub}\"\n"),
    )
    .expect("record");
    let err = signer_for(&ctx, &keys.roster)
        .expect_err("crossed")
        .to_string();
    assert!(
        err.contains("Two provisioning runs have been crossed"),
        "{err}"
    );
}

#[test]
fn a_missing_key_names_the_mint_and_a_missing_record_gives_its_lines() {
    let tmp = scratch("id-missing");
    let keys = Keys::mint("m3", &[]);
    let ctx = keys.ctx(tmp.path(), Arc::new(FakeHost::new()));
    std::fs::remove_file(&ctx.identity.record).expect("rm record");
    let err = signer_for(&ctx, &keys.roster)
        .expect_err("no record")
        .to_string();
    assert!(err.contains("no machine record"), "{err}");
    assert!(
        err.contains(&format!("pubkey = \"{}\"", keys.machine_pub)),
        "the refusal gives the key's own pubkey line: {err}"
    );
    assert!(
        !err.contains("atpkg-keys join"),
        "join cannot restore a record: {err}"
    );
    assert!(
        !err.contains(&tmp.path().display().to_string()),
        "no home path: {err}"
    );

    std::fs::remove_file(&ctx.identity.key).expect("rm key");
    let err = signer_for(&ctx, &keys.roster)
        .expect_err("no key")
        .to_string();
    assert!(err.contains("atpkg-keys setup --id"), "{err}");
}
