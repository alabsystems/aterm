// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the release-credentials profile + the Dev-ID refusal preflights
//! (release spec §6 `sign.rs`, absorbing tools/release-conf.sh +
//! apps/aterm-mac/notarize.sh):
//!   * the ONE credentials profile named by `--release-credentials`;
//!   * the ported stat refusals (ownership + group/other-writable), proven
//!     against real fixture files with bad modes;
//!   * the notarization preflights (reject ad-hoc, require Developer ID
//!     Authority, require hardened runtime on a .app) against captured
//!     `codesign -dv --verbose=2` output shapes.

#[path = "../src/sign.rs"]
#[allow(dead_code)] // the test mount exercises parse/perm/preflight, not the codesign IO
mod sign;

// --- conf parsing ------------------------------------------------------------

#[test]
fn a_profile_yields_the_derived_public_identity() {
    // The profile carries the PRIVATE key; the identity is derived, never declared.
    // The old conf listed ATERM_UPDATE_PUBKEY beside the key path and had to check
    // the two agreed — a whole class of "these disagree" errors that cannot exist
    // when only one of them is written down.
    let dir = tempdir("creds-ok");
    let (pkcs8, pubkey) = keypair_b64();
    let path = write_profile(&dir, &format!("signing_key = \"{pkcs8}\"\n"), 0o600);
    let creds = sign::ReleaseCredentials::load(&path).expect("a 0600 profile loads");
    assert_eq!(creds.pubkey(), pubkey, "identity is derived from the key");
    let sig = creds.sign(b"manifest bytes").expect("signs in-process");
    assert_eq!(sig.len(), 64, "a detached Ed25519 signature is 64 bytes");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_group_or_other_readable_profile_is_refused() {
    // It holds a private key, so the ownership guarantee release.conf carried is
    // kept verbatim rather than lost with the file.
    let dir = tempdir("creds-0644");
    let (pkcs8, _) = keypair_b64();
    let path = write_profile(&dir, &format!("signing_key = \"{pkcs8}\"\n"), 0o644);
    let err = sign::ReleaseCredentials::load(&path).expect_err("0644 must be refused");
    assert!(err.contains("group/other-accessible"), "{err}");
    assert!(
        err.contains("private key"),
        "the refusal must say why: {err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_missing_profile_is_a_hard_error_not_a_silent_none() {
    // INVERTS the old `absent_conf_is_ok_none_not_an_error`. An absent conf used to
    // mean "unsigned is fine", which is exactly how v0.16.0 shipped unsigned. A path
    // that was named and does not exist is a mistake, and says so.
    let err = sign::ReleaseCredentials::load(std::path::Path::new(
        "/nonexistent/aterm-release-credentials.toml",
    ))
    .expect_err("a named-but-missing profile must not read as unsigned");
    assert!(err.contains("read"), "{err}");
}

#[test]
fn a_raw_binary_key_is_refused_by_name() {
    // atpkg-keys writes BINARY PKCS#8. Pasting those bytes into TOML cannot
    // round-trip, and the refusal must say so rather than surface as a parse error
    // the operator cannot act on.
    let dir = tempdir("creds-raw");
    let path = write_profile(&dir, "signing_key = \"not base64 at all!!\"\n", 0o600);
    let err = sign::ReleaseCredentials::load(&path).expect_err("raw bytes must be refused");
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
    let err = sign::ReleaseCredentials::load(&path).expect_err("no key must be refused");
    assert!(err.contains("signing_key"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_private_key_never_reaches_a_debug_line() {
    // Debug is hand-written precisely so a stray `{:?}` cannot leak the key.
    let dir = tempdir("creds-debug");
    let (pkcs8, pubkey) = keypair_b64();
    let path = write_profile(&dir, &format!("signing_key = \"{pkcs8}\"\n"), 0o600);
    let creds = sign::ReleaseCredentials::load(&path).unwrap();
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

// ---- helpers -------------------------------------------------------------

fn tempdir(label: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("aterm-signconf-{label}-{}", std::process::id()));
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

/// A fresh Ed25519 keypair as (base64 PKCS#8, base64 public key).
fn keypair_b64() -> (String, String) {
    use base64::Engine as _;
    use ring::signature::KeyPair as _;
    let rng = ring::rand::SystemRandom::new();
    let doc = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
    let kp = ring::signature::Ed25519KeyPair::from_pkcs8(doc.as_ref()).unwrap();
    let b64 = base64::engine::general_purpose::STANDARD;
    (
        b64.encode(doc.as_ref()),
        b64.encode(kp.public_key().as_ref()),
    )
}

const ADHOC_INFO: &str = "Executable=/tmp/dist/aterm.app/Contents/MacOS/aterm\n\
     Identifier=com.aterm.aterm\n\
     Format=app bundle with Mach-O universal (x86_64 arm64)\n\
     CodeDirectory v=20400 size=1234 flags=0x2(adhoc) hashes=38+7 location=embedded\n\
     Signature=adhoc\n\
     Info.plist entries=15\n";

/// Developer-ID signed WITH the hardened runtime (notarizable).
const DEVID_RUNTIME_INFO: &str = "Executable=/tmp/dist/aterm.app/Contents/MacOS/aterm\n\
     Identifier=com.aterm.aterm\n\
     CodeDirectory v=20500 size=1234 flags=0x10000(runtime) hashes=38+7 location=embedded\n\
     Signature size=8980\n\
     Authority=Developer ID Application: Jane Doe (TEAMID)\n\
     Authority=Developer ID Certification Authority\n\
     Authority=Apple Root CA\n\
     Timestamp=6 Jul 2026 at 12:00:00\n";

/// Developer-ID signed WITHOUT the hardened runtime (Apple would reject it).
const DEVID_NO_RUNTIME_INFO: &str = "Executable=/tmp/dist/aterm.app/Contents/MacOS/aterm\n\
     Identifier=com.aterm.aterm\n\
     CodeDirectory v=20400 size=1234 flags=0x0(none) hashes=38+7 location=embedded\n\
     Signature size=8980\n\
     Authority=Developer ID Application: Jane Doe (TEAMID)\n\
     Authority=Developer ID Certification Authority\n\
     Authority=Apple Root CA\n";

#[test]
fn preflight_rejects_adhoc_signature() {
    // Reject loudly instead of wasting a round-trip to Apple.
    let err = sign::devid_preflight(ADHOC_INFO, true).unwrap_err();
    assert!(err.contains("ad-hoc"), "{err}");
    let err = sign::devid_preflight(ADHOC_INFO, false).unwrap_err();
    assert!(err.contains("ad-hoc"), "{err}");
}

#[test]
fn preflight_rejects_missing_devid_authority() {
    // Unsigned / non-Dev-ID output has no Authority line at all.
    let err = sign::devid_preflight("code object is not signed at all\n", false).unwrap_err();
    assert!(err.contains("not Developer-ID signed"), "{err}");
}

#[test]
fn preflight_requires_hardened_runtime_for_an_app() {
    // .app without "(runtime)" in the CodeDirectory flags → refused BEFORE
    // burning an Apple round-trip on a build that will come back rejected.
    let err = sign::devid_preflight(DEVID_NO_RUNTIME_INFO, true).unwrap_err();
    assert!(err.contains("hardened runtime"), "{err}");
    // The same signature on a DMG is fine — the runtime flag is an app
    // property (notarize.sh scopes the check to *.app).
    sign::devid_preflight(DEVID_NO_RUNTIME_INFO, false).expect("dmg without runtime flag is ok");
}

#[test]
fn preflight_accepts_a_notarizable_devid_app() {
    sign::devid_preflight(DEVID_RUNTIME_INFO, true).expect("devid + runtime app");
    sign::devid_preflight(DEVID_RUNTIME_INFO, false).expect("devid dmg");
}

// --- one-path resolution (machine state as the default identity) -------------

/// The three-way resolution contract: flag wins; machine state is the default;
/// neither is a clean None. Drives `resolve_with` (the injected-home seam), on
/// scratch directories, so it neither reads nor depends on this machine's real
/// provisioning state.
#[test]
fn resolution_prefers_the_flag_then_machine_state_then_none() {
    use std::os::unix::fs::PermissionsExt as _;
    let home = tempdir("resolve-home");
    let repo = tempdir("resolve-repo");
    // Nothing provisioned: None, not an error — the ordinary unsigned cut.
    let none = sign::ReleaseCredentials::resolve_with(None, &repo, home.to_str())
        .expect("an unprovisioned machine resolves clean");
    assert!(none.is_none(), "no flag + no machine key = no credentials");

    // Provisioned: the machine key is picked up, identity derived from the raw
    // PKCS#8 bytes — no base64 hop.
    let aterm = home.join(".aterm");
    std::fs::create_dir_all(&aterm).unwrap();
    let rng = ring::rand::SystemRandom::new();
    let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
    let expected = {
        use base64::Engine as _;
        use ring::signature::KeyPair as _;
        let kp = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        base64::engine::general_purpose::STANDARD.encode(kp.public_key().as_ref())
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

    // A staged roster in the repo becomes the default roster path.
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

    // The flag WINS over machine state: same explicit-profile path as ever.
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
}

/// Fail closed on a HALF-provisioned machine: a key that exists but is
/// group/other-readable is an ERROR naming the file — never a silent
/// fall-through to an unsigned cut, which would demote a custody problem
/// to a missing-signature surprise three steps later.
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
}
