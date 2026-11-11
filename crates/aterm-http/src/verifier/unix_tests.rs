// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn provider() -> Arc<CryptoProvider> {
    crate::tls::init_crypto();
    rustls::crypto::CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| Arc::new(crate::tls::provider()))
}

#[test]
fn an_empty_system_store_is_a_loud_failure_not_a_silent_verifier() {
    let error = Verifier::from_ders(Vec::new(), Vec::new(), provider())
        .expect_err("an empty store must not produce a verifier");
    assert!(
        error.to_string().contains("system trust store"),
        "unhelpful error: {error}"
    );
}

#[test]
fn unparseable_system_roots_are_skipped_but_do_not_take_the_store_with_them() {
    let good = include_bytes!("../testdata/tls/root.der").to_vec();
    let junk = vec![0u8; 8];
    Verifier::from_ders(Vec::new(), vec![junk, good], provider())
        .expect("one usable root among the rubbish is still a usable store");
}

#[test]
fn an_unusable_extra_anchor_is_an_error_rather_than_a_skip() {
    let good = include_bytes!("../testdata/tls/root.der").to_vec();
    assert!(
        Verifier::from_ders(
            vec![CertificateDer::from(vec![0u8; 8])],
            vec![good],
            provider()
        )
        .is_err()
    );
}

#[test]
fn the_built_in_search_finds_this_machines_real_bundle() {
    let file = bundle_file(None);
    let dirs = bundle_dirs(None);
    if file.is_none() && dirs.is_empty() {
        aterm_spec::host::could_not_run("this host has no OpenSSL-shaped CA store");
    }
    let ders = collect_ders(file.as_deref(), &dirs);
    assert!(
        ders.len() > 20,
        "found only {} certificates in {file:?} + {dirs:?}; a real CA store has hundreds",
        ders.len()
    );
    let verifier = Verifier::from_ders(Vec::new(), ders, provider())
        .expect("the host's own CA store must build a verifier");
    let leaf = CertificateDer::from(include_bytes!("../testdata/tls/gh-leaf.der").as_slice());
    let int0 = CertificateDer::from(include_bytes!("../testdata/tls/gh-int0.der").as_slice());
    let int1 = CertificateDer::from(include_bytes!("../testdata/tls/gh-int1.der").as_slice());
    let name = ServerName::try_from("github.com").expect("a usable server name");
    let at = UnixTime::since_unix_epoch(std::time::Duration::from_secs(1_787_986_773));
    let accepted = verifier
        .verify(
            &leaf,
            &[int0.clone(), int1.clone()],
            &name,
            "github.com",
            None,
            at,
        )
        .is_ok();
    if !accepted {
        aterm_spec::host::could_not_run(
            "this host's OpenSSL bundle does not carry the anchor for the captured \
             github.com chain",
        );
    }
    let wrong = ServerName::try_from("example.invalid").expect("a usable server name");
    assert!(
        verifier
            .verify(&leaf, &[int0, int1], &wrong, "example.invalid", None, at)
            .is_err(),
        "the system store accepted a real chain for a name it does not carry"
    );
}

#[test]
fn the_shipped_constructor_reads_this_hosts_store() {
    match Verifier::new(Vec::new(), provider()) {
        Ok(_) => {}
        Err(error) => {
            assert!(
                error.to_string().contains("system trust store"),
                "unexpected failure: {error}"
            );
            aterm_spec::host::could_not_run("this host has no OpenSSL-shaped CA store");
        }
    }
}

/// The environment replaces the built-in search; it does not add to it.
#[test]
fn a_configured_path_replaces_the_built_in_search() {
    let missing = PathBuf::from("/nonexistent/aterm/definitely/not/here.pem");
    assert_eq!(bundle_file(Some(missing.clone())), None);
    assert!(bundle_dirs(Some(missing)).is_empty());
    let real = aterm_tempfile::manifest_dir().join("src/testdata/tls/root.der");
    assert!(
        real.exists(),
        "the fixture must be where the test says it is"
    );
    assert_eq!(bundle_file(Some(real.clone())), Some(real.clone()));
    let dir = aterm_tempfile::manifest_dir().join("src/testdata/tls");
    assert_eq!(bundle_file(Some(dir.clone())), None);
    let mut capped = Vec::new();
    read_pem_file_up_to(&real, 4, &mut capped);
    read_pem_file_up_to(&dir, MAX_PEM_FILE_BYTES, &mut capped);
    assert!(capped.is_empty());
}

/// `$SSL_CERT_DIR` is a `:`-separated list, as OpenSSL and
/// `rustls-native-certs` read it.
#[test]
fn a_configured_cert_dir_is_a_list_of_directories() {
    let testdata = aterm_tempfile::manifest_dir().join("src/testdata");
    let a = testdata.to_str().expect("a UTF-8 package directory");
    let b = &format!("{a}/tls");
    let file = &format!("{a}/tls/root.der");
    let both = vec![PathBuf::from(a), PathBuf::from(b)];
    assert_eq!(bundle_dirs(Some(PathBuf::from(format!("{a}:{b}")))), both);
    assert_eq!(
        bundle_dirs(Some(PathBuf::from(format!(
            "{a}:/nonexistent/aterm/certs:{file}::{b}"
        )))),
        both
    );
}
