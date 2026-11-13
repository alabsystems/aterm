// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{DrClass, classify_dr, devid_preflight};

/// A Developer-ID app's DR (macOS 26.6.2): an identifier clause plus an anchor and
/// no cdhash, the shape a shipped aterm.app must have.
const DEVID_DR: &str = "Executable=/Applications/Ghostty.app/Contents/MacOS/ghostty\n\
     designated => identifier \"com.mitchellh.ghostty\" and anchor apple generic and \
     certificate 1[field.1.2.840.113635.100.6.2.6] /* exists */ and \
     certificate leaf[field.1.2.840.113635.100.6.1.13] /* exists */ and \
     certificate leaf[subject.OU] = \"24VZTF6M5V\"\n";

/// The other real Developer-ID spelling: anchor first, unquoted team OU.
const DEVID_DR_ANCHOR_FIRST: &str = "Executable=/Applications/iTerm.app/Contents/MacOS/iTerm2\n\
     designated => anchor apple generic and identifier \"com.googlecode.iterm2\" and \
     (certificate leaf[field.1.2.840.113635.100.6.1.9] /* exists */ or \
     certificate 1[field.1.2.840.113635.100.6.2.6] /* exists */ and \
     certificate leaf[field.1.2.840.113635.100.6.1.13] /* exists */ and \
     certificate leaf[subject.OU] = H7V7XYVQ7D)\n";

/// `codesign --force --sign - <binary>` (`ship dev-app` without the "aterm dev"
/// identity): a derived (`# `) requirement with no identifier clause.
const ADHOC_DR: &str = "Executable=/tmp/adhoc-probe\n\
     # designated => cdhash H\"240c6c45749cff92effe2dbe2782cd3ac8bee253\" or \
     cdhash H\"458a3ae87054006e9463429282897dfafa67e862\"\n";

/// Same, signed with `--identifier com.aterm.aterm.dev`: the identifier does not
/// reach the requirement, so a dev bundle id alone buys no durable grant.
const ADHOC_DR_WITH_IDENTIFIER: &str = "Executable=/tmp/adhoc-probe2\n\
     # designated => cdhash H\"291d5a2ba2302d7e67699538f48684c2a1e3babf\" or \
     cdhash H\"bcbfaab1440e2f97f26f0d7b31b9e4710f647918\"\n";

/// `codesign -d -r-` on an unsigned Mach-O: one stderr line, no requirement.
const UNSIGNED_DR: &str = "/tmp/unsigned-probe: code object is not signed at all\n";

#[test]
fn classifies_the_recorded_shapes() {
    assert_eq!(classify_dr(DEVID_DR), DrClass::Identity);
    assert_eq!(classify_dr(DEVID_DR_ANCHOR_FIRST), DrClass::Identity);
    assert_eq!(classify_dr(ADHOC_DR), DrClass::Cdhash);
    assert_eq!(classify_dr(ADHOC_DR_WITH_IDENTIFIER), DrClass::Cdhash);
    assert_eq!(classify_dr(UNSIGNED_DR), DrClass::Unsigned);
    assert_eq!(classify_dr(""), DrClass::Unknown);
    assert_eq!(
        classify_dr("designated => identifier \"com.aterm.aterm\"\n"),
        DrClass::Unknown
    );
}

/// A self-signed identity's requirement (`certificate leaf` for `ship dev-app`'s
/// "aterm dev", `certificate root` for a certificate with an Organization) pins the
/// certificate, not the code's bytes, so a grant against it survives a rebuild.
#[test]
fn a_self_signed_identity_requirement_is_identity() {
    for requirement in [
        "designated => identifier \"com.aterm.aterm.dev\" and certificate leaf = \
         H\"e11d6156e11d6156e11d6156e11d6156e11d6156\"\n",
        "designated => identifier \"com.aterm.aterm.dev\" and certificate root = \
         H\"0123456789abcdef0123456789abcdef01234567\"\n",
    ] {
        assert_eq!(classify_dr(requirement), DrClass::Identity, "{requirement}");
    }
    let mixed = "designated => identifier \"com.aterm.aterm\" and anchor apple generic \
         and cdhash H\"deadbeef\"\n";
    assert_eq!(classify_dr(mixed), DrClass::Cdhash);
    let other = "host => cdhash H\"deadbeef\"\nlibrary => cdhash H\"feedface\"\n";
    assert_eq!(classify_dr(other), DrClass::Unknown);
    let both = "host => cdhash H\"deadbeef\"\n\
         designated => identifier \"x\" and anchor apple generic\n";
    assert_eq!(classify_dr(both), DrClass::Identity);
}

#[test]
fn preflight_refuses_a_cdhash_designated_requirement() {
    let info = format!(
        "Identifier=com.aterm.aterm\n\
         CodeDirectory v=20500 size=1234 flags=0x10000(runtime) hashes=38+7 location=embedded\n\
         Signature size=8980\n\
         Authority=Developer ID Application: Jane Doe (TEAMID)\n\
         {ADHOC_DR}"
    );
    let err = devid_preflight(&info, true).unwrap_err();
    assert!(err.contains("cdhash-class"), "{err}");
    assert!(devid_preflight(&info, false).is_err());
}

#[test]
fn preflight_accepts_an_identity_designated_requirement() {
    let info = format!(
        "Identifier=com.aterm.aterm\n\
         CodeDirectory v=20500 size=1234 flags=0x10000(runtime) hashes=38+7 location=embedded\n\
         Signature size=8980\n\
         Authority=Developer ID Application: Jane Doe (TEAMID)\n\
         {DEVID_DR}"
    );
    devid_preflight(&info, true).expect("identity-class DR ships");
    devid_preflight(&info, false).expect("identity-class DR ships (dmg)");
}

#[test]
fn requirement_free_text_does_not_invent_a_refusal() {
    let no_probe = "Identifier=com.aterm.aterm\n\
         CodeDirectory v=20500 size=1234 flags=0x10000(runtime) hashes=38+7 location=embedded\n\
         Signature size=8980\n\
         Authority=Developer ID Application: Jane Doe (TEAMID)\n";
    devid_preflight(no_probe, true).expect("no DR probe in the text is not a refusal");
    devid_preflight(UNSIGNED_DR, true).unwrap_err();
}

#[cfg(unix)]
#[test]
fn release_credentials_refuse_symlinks() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = aterm_tempfile::tempdir().unwrap();
    let real = dir.path().join("real.toml");
    std::fs::write(&real, "signing_key = \"abc\"\n").unwrap();
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
    let link = dir.path().join("profile.toml");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let err = super::ReleaseCredentials::load(&link, Some(dir.path())).unwrap_err();
    assert!(err.contains("not a regular file"), "{err}");

    let key_path = dir.path().join(atpkg_keys::provision::MACHINE_KEY_REL);
    std::fs::create_dir_all(key_path.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&real, &key_path).unwrap();
    let err = super::ReleaseCredentials::resolve_with(
        None,
        dir.path(),
        Some(dir.path().to_str().unwrap()),
    )
    .unwrap_err();
    assert!(err.contains("not a regular file"), "{err}");
}
