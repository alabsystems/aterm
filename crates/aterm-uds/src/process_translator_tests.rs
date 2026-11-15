// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn a_refused_brand_probe_cannot_make_a_known_architecture_mismatch_native() {
    let unknown = super::translator_named("aarch64\n", "x86_64", || {
        Err(std::io::Error::from_raw_os_error(libc::EMFILE))
    });
    assert!(unknown.is_some());
    assert_ne!(unknown.as_deref(), Some("Rosetta"));
    assert_eq!(
        super::translator_named("aarch64", "x86_64", || Ok(true)).as_deref(),
        Some("Rosetta"),
    );
    assert!(
        super::translator_named("aarch64", "aarch64", || {
            panic!("native architecture must not enter the opaque brand probe")
        })
        .is_none()
    );
}
