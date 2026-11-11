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
