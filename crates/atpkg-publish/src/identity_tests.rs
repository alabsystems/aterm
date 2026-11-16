// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::sync::Arc;

/// `signer_for` reads `~/.aterm/release-ring/machine.toml` and ignores `~/.aterm/machine.toml`
/// (`id != m22` in 0.146).
/// NEGATIVE CONTROL: reading `~/.aterm/machine.toml` sees `"m99"` instead of `"m3"` and
/// refuses it: its pubkey is not the key's.
#[test]
fn signer_for_reads_release_ring_and_ignores_host_machine_toml() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let home = dir.path();

    let ring = crate::testkit::Keys::mint("m3", &[]);
    let host = Arc::new(crate::host::fake::FakeHost::default());
    let ctx = ring.ctx(home, host);
    std::fs::write(
        home.join(".aterm/machine.toml"),
        "id = \"m99\"\npubkey = \"wrong-pubkey\"\n",
    )
    .expect("write host machine.toml");
    let signer = signer_for(&ctx, &ring.roster).expect("release-ring record signs");
    assert_eq!(signer.record.id, "m3");

    let mut secret = vec![0xaa, 0x55, 0xff, 0x01];
    scrub_bytes(&mut secret);
    assert_eq!(secret, [0, 0, 0, 0]);

    let oversized = home.join("oversized.key");
    let max_len = usize::try_from(MAX_KEY_BYTES).expect("usize") + 1;
    std::fs::write(&oversized, vec![0x42u8; max_len]).expect("write oversized");
    let err = MachineKey::load(&ctx, &oversized)
        .expect_err("oversized key must fail")
        .to_string();
    assert!(err.contains("exceeds"), "unexpected error: {err}");
    let err_dir = MachineKey::load(&ctx, home)
        .expect_err("directory must fail")
        .to_string();
    assert!(
        err_dir.contains("not a regular file"),
        "unexpected error: {err_dir}"
    );
}

/// THE STORE'S RELEASE RING SIGNS FIRST. Where this Mac's store holds a ring, `signer_for` signs
/// with its key and record, read in place, over the files `atpkg-keys` minted; with neither files
/// nor a store the lane lacks its key by name. NEGATIVE CONTROL: with the store not consulted (an
/// explicit key was named) the minted files sign, and their key is not on the store ring's roster.
#[cfg(unix)]
#[test]
fn the_stores_release_ring_signs_before_the_minted_files() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let store = crate::testkit::Keys::mint("m3", &[]);
    let files = crate::testkit::Keys::mint("m4", &[]);

    let paths = aterm_secrets::client::Paths::for_home(home);
    let source = home.join("team.vault");
    aterm_secrets::client::create(&paths, source.to_str().unwrap(), None, Some("m3")).unwrap();
    let admin = aterm_secrets::client::admin_open(&paths).unwrap();
    let mut doc = admin.doc.clone();
    let record = format!("id = \"m3\"\npubkey = \"{}\"\n", store.machine_pub);
    for (name, value) in [
        (STORE_RING[0], store.machine_key.as_slice()),
        (STORE_RING[1], record.as_bytes()),
    ] {
        doc.set_entry(&aterm_secrets::text::Audience::Everyone, name, value)
            .unwrap();
    }
    aterm_secrets::client::admin_publish(&paths, &admin, &doc).unwrap();

    // The minted files, of another key, written after the store's.
    let host = Arc::new(crate::host::fake::FakeHost::default());
    let mut ctx = files.ctx(home, host);
    let signer = signer_for(&ctx, &store.roster).expect("the store's ring signs");
    assert_eq!(signer.record.id, "m3");
    assert_eq!(signer.key.pubkey_b64().unwrap(), store.machine_pub);
    assert_eq!(missing(&ctx).unwrap(), None);
    assert_eq!(key_shown(&ctx), STORE_RING_SHOWN);

    ctx.identity.store = None;
    let refused = signer_for(&ctx, &store.roster)
        .expect_err("the minted files' key is not on the store ring's roster")
        .to_string();
    assert!(refused.contains("'m4' is NOT on the roster"), "{refused}");
    std::fs::remove_file(&ctx.identity.key).unwrap();
    assert_eq!(missing(&ctx).unwrap(), Some("machine key"));

    // The store's signed roster pair is a candidate of its own, read in place.
    use crate::channel::index::{RosterCandidate, newest_roster};
    let in_store = RosterCandidate::Store(home.to_path_buf());
    let mut log = crate::channel::Log::echo("test");
    assert!(newest_roster(&ctx, std::slice::from_ref(&in_store), &mut log).is_err());
    let admin = aterm_secrets::client::admin_open(&paths).unwrap();
    let mut doc = admin.doc.clone();
    let [roster, sig] = aterm_secrets::entry_class::PUBLIC_ROSTER;
    for (name, value) in [
        (roster, files.roster_bytes.as_slice()),
        (sig, files.roster_sig.as_slice()),
    ] {
        doc.set_entry(&aterm_secrets::text::Audience::Everyone, name, value)
            .unwrap();
    }
    aterm_secrets::client::admin_publish(&paths, &admin, &doc).unwrap();
    let pair =
        newest_roster(&ctx, std::slice::from_ref(&in_store), &mut log).expect("the store's pair");
    assert_eq!(pair.source, in_store);
    assert_eq!(pair.bytes, files.roster_bytes);
}
