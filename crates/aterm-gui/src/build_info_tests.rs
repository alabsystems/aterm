// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The section record and the runtime updater derive the pin independently from one
/// compile input; the release cutter makes the same comparison on shipped bytes.
#[test]
fn embedded_update_pin_matches_runtime_authority() {
    assert_eq!(EMBEDDED_UPDATE_PIN_SHA256.len(), 64);
    assert!(
        EMBEDDED_UPDATE_PIN_SHA256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    );

    let runtime = aterm_update::compiled_update_pin_sha256();
    if runtime == "empty" {
        assert_eq!(EMBEDDED_UPDATE_PIN_SHA256, "0".repeat(64));
    } else {
        assert_eq!(runtime, EMBEDDED_UPDATE_PIN_SHA256);
    }

    #[cfg(target_vendor = "apple")]
    assert_eq!(
        std::str::from_utf8(&ATERM_UPDATE_PIN_RECORD),
        Ok(EMBEDDED_UPDATE_PIN_SHA256)
    );
}

/// The which-copy rows: `running` names the running copy; `another copy` appears only when
/// there is one, every copy on the one row; a bare binary (every test run) names the other
/// copy and promises nothing about it.
#[test]
fn which_copy_rows_name_the_running_copy_and_every_other() {
    use aterm_update::which_copy::{OtherCopy, Running, WhichCopy};
    let alone = WhichCopy {
        running: std::path::PathBuf::from("/Applications/aterm.app"),
        kind: Running::InstalledApp,
        others: Vec::new(),
    };
    assert_eq!(
        which_copy_rows(&alone),
        vec![("running", "/Applications/aterm.app".to_string())]
    );
    let other = |path: &str, version: Option<&str>| OtherCopy {
        path: std::path::PathBuf::from(path),
        version: version.map(str::to_string),
    };
    let two = WhichCopy {
        others: vec![
            other("/Users//ana/Applications/aterm.app", Some("0.60.0")),
            other("/opt/homebrew/Caskroom/aterm/0.59.0/aterm.app", None),
        ],
        ..alone.clone()
    };
    let rows = which_copy_rows(&two);
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[1].0, "another copy");
    assert!(
        rows[1]
            .1
            .contains("/Users//ana/Applications/aterm.app (0.60.0)")
            && rows[1]
                .1
                .contains("/opt/homebrew/Caskroom/aterm/0.59.0/aterm.app"),
        "{rows:?}"
    );
    let binary = WhichCopy {
        running: std::path::PathBuf::from("/Users//ana/aterm/target/release/aterm"),
        kind: Running::Binary,
        others: vec![other("/Applications/aterm.app", Some("0.60.0"))],
    };
    assert_eq!(
        which_copy_rows(&binary)[1],
        (
            "another copy",
            "/Applications/aterm.app (0.60.0)".to_string()
        )
    );
}

/// The ctl line stays `key=value` and carries the compiler keys, `trust=` and
/// `rust_compat=` included.
#[test]
fn control_line_carries_compiler_provenance() {
    let line = control_line();
    assert!(line.starts_with("OK version="), "framing preserved");
    let value = |key: &str| {
        line.split(' ')
            .find_map(|kv| kv.strip_prefix(key))
            .map(str::trim_end)
            .unwrap_or_else(|| panic!("no {key} in {line}"))
    };
    assert_eq!(value("rust_compat="), value("trustc="));
    assert_eq!(value("rust_compat="), compiler_release());
    match compiler_flavor() {
        "t" if compiler_trust_version().is_empty() => {
            assert_eq!(value("trust="), "unreported");
        }
        "t" => assert_eq!(value("trust="), compiler_trust_version()),
        _ => assert_eq!(value("trust="), "none"),
    }
    for key in [
        "build=",
        "commit=",
        "built=",
        "trustc=",
        "trustc_commit=",
        "trustc_host=",
        "flavor=",
        "trust=",
        "rust_compat=",
        "profile=",
        "trust_verify=",
        "update_pin_sha256=",
        "signature=",
    ] {
        assert!(line.contains(&format!(" {key}")), "has {key}: {line}");
    }
    assert!(
        line.trim_end()
            .split(' ')
            .skip(1)
            .all(|kv| kv.contains('=')),
        "every token after OK is key=value: {line}"
    );
}

/// On macOS the signature is the head of the running code's code directory hash, which
/// the kernel holds from the exec, not a hash of the file: re-derived here from this test
/// binary's embedded signature (SHA-256 over its SHA-256 code directory, the cdhash
/// `codesign -dvvv` prints), which an FxHash of the executable's bytes never equals.
#[cfg(target_os = "macos")]
#[test]
fn signature_is_the_running_code_directory_hash() {
    let exe = std::fs::read(std::env::current_exe().expect("current_exe")).expect("read");
    let le = |o: usize| u32::from_le_bytes(exe[o..o + 4].try_into().expect("4 bytes"));
    let be = |b: &[u8], o: usize| u32::from_be_bytes(b[o..o + 4].try_into().expect("4 bytes"));
    assert_eq!(le(0), 0xfeed_facf, "a thin 64-bit Mach-O");
    let mut at = 32;
    let mut blob = None;
    for _ in 0..le(16) {
        if le(at) == 0x1d {
            let off = le(at + 8) as usize;
            blob = Some(&exe[off..off + le(at + 12) as usize]);
        }
        at += le(at + 4) as usize;
    }
    let Some(blob) = blob else {
        assert!(
            binary_signature_reads_the_file(),
            "no code directory to read"
        );
        assert_eq!(
            binary_signature(),
            format!("{:016x}", aterm_hash::fx_hash_bytes(&exe))
        );
        return;
    };
    assert!(
        !binary_signature_reads_the_file(),
        "a signed binary's signature is the kernel's"
    );
    assert_eq!(be(blob, 0), 0xfade_0cc0, "CSMAGIC_EMBEDDED_SIGNATURE");
    let directory = (0..be(blob, 8) as usize)
        .map(|i| &blob[be(blob, 16 + 8 * i) as usize..])
        .filter(|b| be(b, 0) == 0xfade_0c02)
        .map(|b| &b[..be(b, 4) as usize])
        .find(|b| b[37] == 2)
        .expect("a SHA-256 code directory (CS_HASHTYPE_SHA256 at byte 37)");
    let cdhash = aterm_digest::Sha256::digest(directory);
    assert_eq!(
        binary_signature(),
        aterm_codec::hex::encode_lower(&cdhash[..8])
    );
}
