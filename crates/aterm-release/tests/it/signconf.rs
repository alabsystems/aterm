// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the release-credentials profile and notarization preflights.

use crate::sign;

#[test]
fn a_profile_yields_the_derived_public_identity() {
    let dir = tempdir("creds-ok");
    let (pkcs8, pubkey) = keypair_b64();
    let path = write_profile(&dir, &format!("signing_key = \"{pkcs8}\"\n"), 0o600);
    let creds = sign::ReleaseCredentials::load(&path, None).expect("a 0600 profile loads");
    assert_eq!(creds.pubkey(), pubkey, "identity is derived from the key");
    let sig = creds.sign(b"manifest bytes").expect("signs in-process");
    assert_eq!(sig.len(), 64, "a detached Ed25519 signature is 64 bytes");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_group_or_other_readable_profile_is_refused() {
    let dir = tempdir("creds-0644");
    let (pkcs8, _) = keypair_b64();
    let path = write_profile(&dir, &format!("signing_key = \"{pkcs8}\"\n"), 0o644);
    let err = sign::ReleaseCredentials::load(&path, None).expect_err("0644 must be refused");
    assert!(err.contains("group/other-accessible"), "{err}");
    assert!(
        err.contains("private key"),
        "the refusal must say why: {err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_missing_profile_is_a_hard_error_not_a_silent_none() {
    let err = sign::ReleaseCredentials::load(
        std::path::Path::new("/nonexistent/aterm-release-credentials.toml"),
        None,
    )
    .expect_err("a named-but-missing profile must not read as unsigned");
    assert!(err.contains("read"), "{err}");
}

#[test]
fn a_raw_binary_key_is_refused_by_name() {
    let dir = tempdir("creds-raw");
    let path = write_profile(&dir, "signing_key = \"not base64 at all!!\"\n", 0o600);
    let err = sign::ReleaseCredentials::load(&path, None).expect_err("raw bytes must be refused");
    assert!(
        err.contains("base64"),
        "the refusal must name the encoding: {err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_profile_without_a_signing_key_is_refused() {
    let dir = tempdir("creds-empty");
    let path = write_profile(&dir, "# nothing here\n", 0o600);
    let err = sign::ReleaseCredentials::load(&path, None).expect_err("no key must be refused");
    assert!(err.contains("signing_key"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_private_key_never_reaches_a_debug_line() {
    let dir = tempdir("creds-debug");
    let (pkcs8, pubkey) = keypair_b64();
    let path = write_profile(&dir, &format!("signing_key = \"{pkcs8}\"\n"), 0o600);
    let creds = sign::ReleaseCredentials::load(&path, None).unwrap();
    let rendered = format!("{creds:?}");
    assert!(
        rendered.contains(&pubkey),
        "the identity is printable: {rendered}"
    );
    assert!(rendered.contains("redacted"), "{rendered}");
    assert!(
        !rendered.contains(&pkcs8),
        "the private key must never render"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn tempdir(label: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("aterm-signconf-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_profile(dir: &std::path::Path, body: &str, mode: u32) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let path = dir.join("release-credentials.toml");
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    path
}

fn keypair_b64() -> (String, String) {
    use ring::signature::KeyPair as _;
    let rng = ring::rand::SystemRandom::new();
    let doc = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
    let kp = ring::signature::Ed25519KeyPair::from_pkcs8(doc.as_ref()).unwrap();
    (
        aterm_codec::base64::encode(doc.as_ref()).expect("pkcs8"),
        aterm_codec::base64::encode(kp.public_key().as_ref()).expect("32-byte key"),
    )
}

const ADHOC_INFO: &str = "Executable=/tmp/dist/aterm.app/Contents/MacOS/aterm\n\
     Identifier=com.aterm.aterm\n\
     Format=app bundle with Mach-O universal (x86_64 arm64)\n\
     CodeDirectory v=20400 size=1234 flags=0x2(adhoc) hashes=38+7 location=embedded\n\
     Signature=adhoc\n\
     Info.plist entries=15\n";

const DEVID_RUNTIME_INFO: &str = "Executable=/tmp/dist/aterm.app/Contents/MacOS/aterm\n\
     Identifier=com.aterm.aterm\n\
     CodeDirectory v=20500 size=1234 flags=0x10000(runtime) hashes=38+7 location=embedded\n\
     Signature size=8980\n\
     Authority=Developer ID Application: Jane Doe (TEAMID)\n\
     Authority=Developer ID Certification Authority\n\
     Authority=Apple Root CA\n\
     Timestamp=6 Jul 2026 at 12:00:00\n";

const DEVID_NO_RUNTIME_INFO: &str = "Executable=/tmp/dist/aterm.app/Contents/MacOS/aterm\n\
     Identifier=com.aterm.aterm\n\
     CodeDirectory v=20400 size=1234 flags=0x0(none) hashes=38+7 location=embedded\n\
     Signature size=8980\n\
     Authority=Developer ID Application: Jane Doe (TEAMID)\n\
     Authority=Developer ID Certification Authority\n\
     Authority=Apple Root CA\n";

#[test]
fn preflight_rejects_adhoc_signature() {
    let err = sign::devid_preflight(ADHOC_INFO, true).unwrap_err();
    assert!(err.contains("ad-hoc"), "{err}");
    let err = sign::devid_preflight(ADHOC_INFO, false).unwrap_err();
    assert!(err.contains("ad-hoc"), "{err}");
}

#[test]
fn preflight_rejects_missing_devid_authority() {
    let err = sign::devid_preflight("code object is not signed at all\n", false).unwrap_err();
    assert!(err.contains("not Developer-ID signed"), "{err}");
}

#[test]
fn preflight_requires_hardened_runtime_for_an_app() {
    let err = sign::devid_preflight(DEVID_NO_RUNTIME_INFO, true).unwrap_err();
    assert!(err.contains("hardened runtime"), "{err}");
    sign::devid_preflight(DEVID_NO_RUNTIME_INFO, false).expect("dmg without runtime flag is ok");
}

#[test]
fn preflight_accepts_a_notarizable_devid_app() {
    sign::devid_preflight(DEVID_RUNTIME_INFO, true).expect("devid + runtime app");
    sign::devid_preflight(DEVID_RUNTIME_INFO, false).expect("devid dmg");
}

#[test]
fn resolution_prefers_the_flag_then_machine_state_then_none() {
    use std::os::unix::fs::PermissionsExt as _;
    let home = tempdir("resolve-home");
    let repo = tempdir("resolve-repo");
    let none = sign::ReleaseCredentials::resolve_with(None, &repo, home.to_str())
        .expect("an unprovisioned machine resolves clean");
    assert!(none.is_none(), "no flag + no machine key = no credentials");

    let aterm = home.join(".aterm");
    std::fs::create_dir_all(&aterm).unwrap();
    let rng = ring::rand::SystemRandom::new();
    let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
    let expected = {
        use ring::signature::KeyPair as _;
        let kp = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        aterm_codec::base64::encode(kp.public_key().as_ref()).expect("32-byte key")
    };
    let key_path = aterm.join("machine.key");
    std::fs::write(&key_path, pkcs8.as_ref()).unwrap();
    std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let machine = sign::ReleaseCredentials::resolve_with(None, &repo, home.to_str())
        .expect("a provisioned machine resolves")
        .expect("machine state yields credentials");
    assert_eq!(
        machine.pubkey(),
        expected,
        "identity derives from machine.key"
    );
    assert!(
        machine.machine_roster().is_none(),
        "no staged roster in the repo means none is claimed"
    );

    std::fs::create_dir_all(repo.join("dist")).unwrap();
    std::fs::write(repo.join("dist/aterm-machines.toml"), "x").unwrap();
    let with_roster = sign::ReleaseCredentials::resolve_with(None, &repo, home.to_str())
        .expect("resolves")
        .expect("credentials");
    assert_eq!(
        with_roster.machine_roster(),
        Some(repo.join("dist/aterm-machines.toml").as_path()),
        "the staged roster is the default — the same one the atpkg tools ship"
    );

    let (pkcs8_b64, flag_pub) = keypair_b64();
    let profile = write_profile(&home, &format!("signing_key = \"{pkcs8_b64}\"\n"), 0o600);
    let flagged = sign::ReleaseCredentials::resolve_with(Some(&profile), &repo, home.to_str())
        .expect("the explicit profile loads")
        .expect("credentials");
    assert_eq!(
        flagged.pubkey(),
        flag_pub,
        "an explicit --release-credentials outranks ambient machine state"
    );
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn a_present_but_unsafe_machine_key_is_an_error_not_a_fallthrough() {
    use std::os::unix::fs::PermissionsExt as _;
    let home = tempdir("resolve-badperm");
    let repo = tempdir("resolve-badperm-repo");
    let aterm = home.join(".aterm");
    std::fs::create_dir_all(&aterm).unwrap();
    let rng = ring::rand::SystemRandom::new();
    let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
    let key_path = aterm.join("machine.key");
    std::fs::write(&key_path, pkcs8.as_ref()).unwrap();
    std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let err = sign::ReleaseCredentials::resolve_with(None, &repo, home.to_str())
        .expect_err("a world-readable machine key must refuse, not skip");
    assert!(
        err.contains("machine.key"),
        "the refusal names the offending file: {err}"
    );
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&repo);
}
