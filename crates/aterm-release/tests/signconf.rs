// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the release.conf surface + the Dev-ID refusal preflights
//! (release spec §6 `sign.rs`, absorbing tools/release-conf.sh +
//! apps/aterm-mac/notarize.sh):
//!   * in-process KEY=value parsing — the file is DATA now, never sourced;
//!   * the ported stat refusals (ownership + group/other-writable), proven
//!     against real fixture files with bad modes;
//!   * the notarization preflights (reject ad-hoc, require Developer ID
//!     Authority, require hardened runtime on a .app) against captured
//!     `codesign -dv --verbose=2` output shapes.

#[path = "../src/sign.rs"]
#[allow(dead_code)] // the test mount exercises parse/perm/preflight, not the codesign IO
mod sign;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// Fixture conf under cargo's per-crate test tmpdir, chmodded to `mode`.
/// (Created by this test process, so it is owned by the current uid — the
/// ownership-mismatch arm is covered via the pure check below; a real
/// foreign-owner file would need root to create.)
#[cfg(unix)] // chmod fixture — Unix-only (load_conf refuses any conf on Windows)
fn fixture(name: &str, body: &str, mode: u32) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("signconf");
    std::fs::create_dir_all(&dir).expect("create fixture dir");
    let path = dir.join(name);
    std::fs::write(&path, body).expect("write fixture");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("chmod fixture");
    path
}

// --- conf parsing ------------------------------------------------------------

#[test]
fn parses_the_documented_key_value_shapes() {
    let vars = sign::parse_conf(
        "# aterm release credentials\n\
         \n\
         ATERM_SIGN_ID=\"Developer ID Application: Jane Doe (TEAMID)\"\n\
         ATERM_TEAM_ID=TEAMID\n\
         export ATERM_PKG_ROOTKEY='cm9vdGtleQ=='\n\
         ATERM_UPDATE_SIGN_KEY=/Users//jane/.aterm/update.key\n",
    )
    .expect("parse");
    let get = |k: &str| {
        vars.iter()
            .rev()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.as_str())
    };
    // Quotes stripped (both kinds), `export` prefix accepted, literals verbatim.
    assert_eq!(
        get("ATERM_SIGN_ID"),
        Some("Developer ID Application: Jane Doe (TEAMID)")
    );
    assert_eq!(get("ATERM_TEAM_ID"), Some("TEAMID"));
    assert_eq!(get("ATERM_PKG_ROOTKEY"), Some("cm9vdGtleQ=="));
    assert_eq!(
        get("ATERM_UPDATE_SIGN_KEY"),
        Some("/Users//jane/.aterm/update.key")
    );
}

#[cfg(unix)]
#[test]
fn last_assignment_wins_like_shell_sourcing() {
    let conf = load_ok("dup.conf", "ATERM_TEAM_ID=OLD\nATERM_TEAM_ID=NEW\n");
    assert_eq!(conf.get("ATERM_TEAM_ID"), Some("NEW"));
}

#[test]
fn refuses_non_assignment_lines_with_line_number() {
    let err = sign::parse_conf("ATERM_TEAM_ID=TEAMID\necho pwned\n").unwrap_err();
    assert!(err.contains("line 2"), "{err}");
    assert!(err.contains("not a KEY=value"), "{err}");
}

#[test]
fn refuses_invalid_key_names() {
    for bad in ["1KEY=x\n", "KEY NAME=x\n", "=x\n", "KE-Y=x\n"] {
        let err = sign::parse_conf(bad).unwrap_err();
        assert!(err.contains("line 1"), "{bad:?}: {err}");
    }
}

#[test]
fn refuses_shell_expansion_in_values() {
    // The sourcing era would have expanded these; parsed-as-data must refuse
    // loudly instead of exporting a silently different value.
    for bad in ["K=$HOME/key\n", "K=\"$(id)\"\n", "K=`id`\n"] {
        let err = sign::parse_conf(bad).unwrap_err();
        assert!(err.contains("shell expansion"), "{bad:?}: {err}");
    }
}

#[cfg(unix)]
#[test]
fn env_pins_exports_exactly_the_three_compile_pins() {
    let conf = load_ok(
        "pins.conf",
        "ATERM_UPDATE_PUBKEY=updpub\n\
         ATERM_PKG_ROOTKEY=rootkey\n\
         ATERM_EXPECTED_TEAM_ID=TEAMID\n\
         ATERM_SIGN_ID=whatever\n\
         ATERM_APP_PASSWORD=secret\n",
    );
    let mut pins = conf.env_pins();
    pins.sort();
    // ONLY the option_env! compile pins reach the child cargo env — never the
    // signing identity or notary secrets (those stay in this process).
    assert_eq!(
        pins,
        vec![
            ("ATERM_EXPECTED_TEAM_ID".to_string(), "TEAMID".to_string()),
            ("ATERM_PKG_ROOTKEY".to_string(), "rootkey".to_string()),
            ("ATERM_UPDATE_PUBKEY".to_string(), "updpub".to_string()),
        ]
    );
}

#[cfg(unix)]
#[test]
fn notary_auth_prefers_keychain_profile_and_requires_the_full_trio() {
    // Profile wins even when the trio is also present (notarize.sh's order).
    let conf = load_ok(
        "auth1.conf",
        "ATERM_NOTARY_PROFILE=aterm-notary\n\
         ATERM_APPLE_ID=jane@example.com\nATERM_TEAM_ID=TEAMID\nATERM_APP_PASSWORD=pw\n",
    );
    assert!(
        matches!(conf.notary_auth(), Some(sign::NotaryAuth::KeychainProfile(p)) if p == "aterm-notary")
    );

    // Full trio without a profile → Apple-ID auth.
    let conf = load_ok(
        "auth2.conf",
        "ATERM_APPLE_ID=jane@example.com\nATERM_TEAM_ID=TEAMID\nATERM_APP_PASSWORD=pw\n",
    );
    assert!(matches!(
        conf.notary_auth(),
        Some(sign::NotaryAuth::AppleId { .. })
    ));

    // Incomplete trio → no credentials (the caller then skips notarization
    // with a clear message; it must never guess).
    let conf = load_ok(
        "auth3.conf",
        "ATERM_APPLE_ID=jane@example.com\nATERM_TEAM_ID=TEAMID\n",
    );
    assert!(conf.notary_auth().is_none());
}

// --- the 0600/ownership refusal (ported release-conf.sh stat checks) ---------

/// Load via the real loader (metadata checks included) or panic.
#[cfg(unix)]
fn load_ok(name: &str, body: &str) -> sign::ReleaseConf {
    let path = fixture(name, body, 0o600);
    sign::load_conf(&path).expect("load").expect("present")
}

#[cfg(unix)]
#[test]
fn mode_0600_is_accepted() {
    let path = fixture("ok600.conf", "ATERM_TEAM_ID=TEAMID\n", 0o600);
    let conf = sign::load_conf(&path).expect("load").expect("present");
    assert_eq!(conf.get("ATERM_TEAM_ID"), Some("TEAMID"));
}

#[cfg(unix)]
#[test]
fn group_or_other_writable_is_refused() {
    // Every write-bit combination release-conf.sh's mode regex rejects.
    for mode in [0o620u32, 0o602, 0o622, 0o660, 0o666, 0o646] {
        let path = fixture(&format!("bad{mode:o}.conf"), "ATERM_TEAM_ID=TEAMID\n", mode);
        let err = sign::load_conf(&path).unwrap_err();
        assert!(err.contains("group/other-writable"), "mode {mode:o}: {err}");
        assert!(err.contains("chmod 600"), "mode {mode:o}: {err}");
    }
}

#[cfg(unix)]
#[test]
fn owner_only_write_bits_pass_the_mode_check() {
    // release-conf.sh refuses WRITABILITY by others (the code-execution vector
    // when it was sourced), not readability — 0644 loads, 0664 does not.
    let path = fixture("ok644.conf", "ATERM_TEAM_ID=TEAMID\n", 0o644);
    assert!(sign::load_conf(&path).expect("load").is_some());
}

#[test]
fn foreign_owner_is_refused_by_the_pure_check() {
    // Creating a file owned by another uid needs root, so the ownership arm
    // is proven on the pure rule the loader calls with real stat values.
    let err = sign::check_conf_perms(0, 0o600, 501, Path::new("/tmp/release.conf")).unwrap_err();
    assert!(err.contains("not owned by you"), "{err}");
    assert!(err.contains("uid 0"), "{err}");

    // Same uid + tight mode passes; the mode arm fires independently.
    sign::check_conf_perms(501, 0o600, 501, Path::new("/tmp/release.conf")).expect("owner ok");
    let err = sign::check_conf_perms(501, 0o666, 501, Path::new("/tmp/release.conf")).unwrap_err();
    assert!(err.contains("group/other-writable"), "{err}");
}

#[test]
fn absent_conf_is_ok_none_not_an_error() {
    // Dev builds without credentials keep the fail-closed defaults — absence
    // must never abort the pipeline (release-conf.sh's final rule).
    let missing =
        PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("signconf/definitely-absent.conf");
    assert!(sign::load_conf(&missing).expect("load").is_none());
}

// --- Dev-ID notarization preflights (notarize.sh's refusals) ------------------

/// codesign -dv --verbose=2 shape for an AD-HOC signed bundle.
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
