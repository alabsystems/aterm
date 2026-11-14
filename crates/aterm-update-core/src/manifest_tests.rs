// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn full() -> Manifest {
    Manifest {
        schema: 1,
        version: "0.26".into(),
        build_number: 1_783_918_101,
        commit: Some("aed5a06caed5a06caed5a06caed5a06caed5a06c".into()),
        dmg: "aterm-0.26.dmg".into(),
        sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into(),
        url: Some(
            "https://github.com/alabsystems/aterm/releases/download/v0.26/aterm-0.26.dmg".into(),
        ),
        zip: Some("aterm-0.26-mac.zip".into()),
        zip_sha256: Some("9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08".into()),
        dmg_x86_64: Some("aterm-0.26-x86_64.dmg".into()),
        dmg_x86_64_sha256: Some(
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into(),
        ),
        linux_x86_64: None,
        linux_x86_64_sha256: None,
        linux_x86_64_size: None,
        linux_aarch64: None,
        linux_aarch64_sha256: None,
        linux_aarch64_size: None,
        linux_verifier_x86_64: None,
        linux_verifier_x86_64_sha256: None,
        linux_verifier_x86_64_size: None,
        linux_verifier_aarch64: None,
        linux_verifier_aarch64_sha256: None,
        linux_verifier_aarch64_size: None,
        linux_verifier_interface: None,
        windows_x86_64: None,
        windows_x86_64_sha256: None,
        windows_x86_64_size: None,
        windows_verifier_x86_64: None,
        windows_verifier_x86_64_sha256: None,
        windows_verifier_x86_64_size: None,
        windows_verifier_interface: None,
        min_os: Some("11.0".into()),
        team_id: Some(String::new()),
        pub_date: Some("2026-07-06T21:29:44Z".into()),
        min_build: None,
        machine_id: None,
        roster_seq: None,
        changelog: Some(
            "### Added\n- a `thing` with \"quotes\", # hashes and a \\ backslash\n".into(),
        ),
    }
}

fn linux_manifest() -> Manifest {
    let mut m = full();
    m.version = "0.86.0".into();
    m.dmg = "aterm-0.86.0.dmg".into();
    m.linux_aarch64 = Some("aterm-0.86.0-linux-aarch64".into());
    m.linux_aarch64_sha256 = Some("ab".repeat(32));
    m.linux_aarch64_size = Some(33_000_000);
    m
}

/// The verifier receipts are signed bytes the client carries and never judges: they
/// round-trip, and a manifest without them reads as none.
#[test]
fn verifier_receipts_round_trip_and_are_absent_by_default() {
    let mut m = linux_manifest();
    assert_eq!(m.linux_verifier_aarch64, None);
    m.linux_verifier_aarch64 = Some("aterm-bootstrap-verify-0.86.0-linux-aarch64".into());
    m.linux_verifier_aarch64_sha256 = Some("cd".repeat(32));
    m.linux_verifier_aarch64_size = Some(2_000_000);
    m.linux_verifier_interface = Some(1);
    let text = m.to_toml().unwrap();
    assert!(
        text.contains("linux_verifier_aarch64_sha256 = \""),
        "{text}"
    );
    assert!(!text.contains("linux_verifier_x86_64"), "{text}");
    assert_eq!(Manifest::parse(&text).unwrap(), m);
}

#[test]
fn linux_entries_round_trip_and_select_only_the_declared_target() {
    use crate::linux::LinuxTarget;
    let mut m = linux_manifest();
    let aarch64 = m.linux_artifact(LinuxTarget::Aarch64).unwrap().unwrap();
    assert_eq!(aarch64.name, "aterm-0.86.0-linux-aarch64");
    assert_eq!(aarch64.size, 33_000_000);
    assert_eq!(aarch64.sha256, "ab".repeat(32));
    assert_eq!(m.linux_artifact(LinuxTarget::X86_64).unwrap(), None);
    assert_eq!(Manifest::parse(&m.to_toml().unwrap()).unwrap(), m);
    m.linux_x86_64 = Some("aterm-0.86.0-linux-x86_64".into());
    m.linux_x86_64_sha256 = Some("cd".repeat(32));
    m.linux_x86_64_size = Some(34_000_000);
    let text = m.to_toml().unwrap();
    assert_eq!(Manifest::parse(&text).unwrap(), m);
    assert_eq!(
        m.linux_artifact(LinuxTarget::X86_64).unwrap().unwrap().size,
        34_000_000
    );
    assert!(text.contains("changelog = '''\n"));
}

#[test]
fn every_partial_linux_entry_fails_closed_on_parse_and_emit() {
    for fields in 1_u8..7 {
        let mut m = linux_manifest();
        if fields & 1 == 0 {
            m.linux_aarch64 = None;
        }
        if fields & 2 == 0 {
            m.linux_aarch64_sha256 = None;
        }
        if fields & 4 == 0 {
            m.linux_aarch64_size = None;
        }
        assert!(m.to_toml().is_err(), "partial mask {fields}");
        let wire = aterm_toml::to_string(&m).unwrap();
        assert!(Manifest::parse(&wire).is_err(), "partial mask {fields}");
    }
}

#[test]
fn linux_identity_digest_size_and_legacy_version_mutants_are_rejected() {
    for name in [
        "aterm-0.85.0-linux-aarch64",
        "aterm-0.86.0-linux-x86_64",
        "aterm-0.86.0-linux-aarch64.tar.gz",
        "../aterm-0.86.0-linux-aarch64",
        "https://example.invalid/aterm",
    ] {
        let mut m = linux_manifest();
        m.linux_aarch64 = Some(name.into());
        assert!(m.to_toml().is_err(), "{name}");
    }
    for digest in ["".into(), "AB".repeat(32), "xy".repeat(32), "ab".repeat(31)] {
        let mut m = linux_manifest();
        m.linux_aarch64_sha256 = Some(digest);
        assert!(m.to_toml().is_err());
    }
    for size in [0, 63, crate::linux::LINUX_BINARY_MAX_BYTES + 1, u64::MAX] {
        let mut m = linux_manifest();
        m.linux_aarch64_size = Some(size);
        assert!(m.to_toml().is_err(), "size {size}");
    }
    for version in ["0.86", "0.086.0", "0.86.0+gabc", "0.86.0/../1"] {
        let mut m = linux_manifest();
        m.version = version.into();
        m.linux_aarch64 = Some(crate::linux::LinuxTarget::Aarch64.asset_name(version));
        assert!(m.to_toml().is_err(), "version {version}");
    }
}

fn windows_manifest() -> Manifest {
    let mut m = linux_manifest();
    m.windows_x86_64 = Some("aterm-0.86.0-windows-x86_64.zip".into());
    m.windows_x86_64_sha256 = Some("ef".repeat(32));
    m.windows_x86_64_size = Some(90_000_000);
    m
}

/// A Windows zip rides in schema 1 beside the Linux executable: it round-trips, its selector
/// reads only its own three fields, and the Linux selector is unmoved.
#[test]
fn windows_entries_round_trip_and_select_only_their_own_fields() {
    use crate::linux::LinuxTarget;
    use crate::windows::WindowsTarget;
    let m = windows_manifest();
    let text = m.to_toml().unwrap();
    assert!(
        text.contains("windows_x86_64 = \"aterm-0.86.0-windows-x86_64.zip\"\n"),
        "{text}"
    );
    let back = Manifest::parse(&text).unwrap();
    assert_eq!(back, m);
    assert_eq!(back.schema, SUPPORTED_SCHEMA);
    let zip = back
        .windows_artifact(WindowsTarget::X86_64)
        .unwrap()
        .unwrap();
    assert_eq!(zip.target, WindowsTarget::X86_64);
    assert_eq!(zip.name, "aterm-0.86.0-windows-x86_64.zip");
    assert_eq!(zip.sha256, "ef".repeat(32));
    assert_eq!(zip.size, 90_000_000);
    let linux = linux_manifest();
    assert_eq!(
        back.linux_artifact(LinuxTarget::Aarch64).unwrap(),
        linux.linux_artifact(LinuxTarget::Aarch64).unwrap()
    );
    assert_eq!(linux.windows_artifact(WindowsTarget::X86_64).unwrap(), None);
}

/// The Windows verifier's receipt is signed bytes no client judges, as the Linux verifiers'
/// are: it round-trips beside the zip, reads as none when absent, and leaves the zip's
/// selector as it was.
#[test]
fn the_windows_verifier_receipt_round_trips_and_is_absent_by_default() {
    let mut m = windows_manifest();
    assert_eq!(
        (
            m.windows_verifier_x86_64.as_deref(),
            m.windows_verifier_interface
        ),
        (None, None)
    );
    assert!(!m.to_toml().unwrap().contains("windows_verifier"));
    let zip = m
        .windows_artifact(crate::windows::WindowsTarget::X86_64)
        .unwrap()
        .map(|a| (a.name.to_string(), a.sha256.to_string(), a.size));
    m.windows_verifier_x86_64 = Some("aterm-bootstrap-verify-0.86.0-windows-x86_64.exe".into());
    m.windows_verifier_x86_64_sha256 = Some("a1".repeat(32));
    m.windows_verifier_x86_64_size = Some(1_800_000);
    m.windows_verifier_interface = Some(1);
    let text = m.to_toml().unwrap();
    for line in [
        "windows_verifier_x86_64 = \"aterm-bootstrap-verify-0.86.0-windows-x86_64.exe\"\n",
        "windows_verifier_x86_64_size = 1800000\n",
        "windows_verifier_interface = 1\n",
    ] {
        assert!(text.contains(line), "{line}: {text}");
    }
    let back = Manifest::parse(&text).unwrap();
    assert_eq!(back, m);
    assert_eq!(
        back.windows_artifact(crate::windows::WindowsTarget::X86_64)
            .unwrap()
            .map(|a| (a.name.to_string(), a.sha256.to_string(), a.size)),
        zip
    );
}

/// A client from before the Windows keys reads a Windows-bearing appcast: its `Manifest` is
/// this serde derive without them, and no appcast reader denies a key it does not name (the
/// derive has no `deny_unknown_fields`; install.sh's and aterm-bootstrap-verify's readers look
/// keys up by name). The keys add no schema: the appcast stays schema 1.
#[test]
fn a_reader_that_predates_the_windows_keys_reads_their_appcast() {
    #[derive(serde::Deserialize)]
    struct Before {
        schema: u32,
        version: String,
        build_number: u64,
        dmg: String,
        linux_aarch64: Option<String>,
        linux_aarch64_size: Option<u64>,
        changelog: Option<String>,
    }
    let mut m = windows_manifest();
    m.windows_verifier_x86_64 = Some("aterm-bootstrap-verify-0.86.0-windows-x86_64.exe".into());
    m.windows_verifier_x86_64_sha256 = Some("a1".repeat(32));
    m.windows_verifier_x86_64_size = Some(1_800_000);
    m.windows_verifier_interface = Some(1);
    let before: Before = aterm_toml::from_str(&m.to_toml().unwrap()).unwrap();
    assert_eq!(before.schema, 1);
    assert_eq!(
        (
            before.version.as_str(),
            before.build_number,
            before.dmg.as_str()
        ),
        (m.version.as_str(), m.build_number, m.dmg.as_str())
    );
    assert_eq!(before.linux_aarch64, m.linux_aarch64);
    assert_eq!(before.linux_aarch64_size, m.linux_aarch64_size);
    assert_eq!(before.changelog, m.changelog);
}

/// An appcast cut before the Windows keys parses unchanged and re-emits with none of them.
#[test]
fn an_appcast_without_windows_keys_reads_as_no_windows_zip() {
    let m = linux_manifest();
    let text = m.to_toml().unwrap();
    assert!(!text.contains("windows"), "{text}");
    let back = Manifest::parse(&text).unwrap();
    assert_eq!(back, m);
    assert_eq!(
        back.windows_artifact(crate::windows::WindowsTarget::X86_64)
            .unwrap(),
        None
    );
    assert_eq!(back.to_toml().unwrap(), text);
}

#[test]
fn every_partial_windows_entry_fails_closed_on_parse_and_emit() {
    for fields in 1_u8..7 {
        let mut m = windows_manifest();
        if fields & 1 == 0 {
            m.windows_x86_64 = None;
        }
        if fields & 2 == 0 {
            m.windows_x86_64_sha256 = None;
        }
        if fields & 4 == 0 {
            m.windows_x86_64_size = None;
        }
        assert!(m.to_toml().is_err(), "partial mask {fields}");
        let wire = aterm_toml::to_string(&m).unwrap();
        assert!(Manifest::parse(&wire).is_err(), "partial mask {fields}");
    }
}

#[test]
fn windows_identity_digest_size_and_version_mutants_are_rejected() {
    for name in [
        "aterm-0.85.0-windows-x86_64.zip",
        "aterm-0.86.0-windows-x86_64",
        "aterm-0.86.0-windows-x86_64.ZIP",
        "aterm-0.86.0-windows-aarch64.zip",
        "aterm-0.86.0-x86_64-portable.zip",
        "aterm-0.86.0-linux-x86_64",
        "../aterm-0.86.0-windows-x86_64.zip",
    ] {
        let mut m = windows_manifest();
        m.windows_x86_64 = Some(name.into());
        assert!(m.to_toml().is_err(), "{name}");
        let wire = aterm_toml::to_string(&m).unwrap();
        assert!(Manifest::parse(&wire).is_err(), "{name}");
    }
    for digest in ["".into(), "EF".repeat(32), "xy".repeat(32), "ef".repeat(31)] {
        let mut m = windows_manifest();
        m.windows_x86_64_sha256 = Some(digest);
        assert!(m.to_toml().is_err());
    }
    for size in [0, 63, crate::windows::WINDOWS_ZIP_MAX_BYTES + 1, u64::MAX] {
        let mut m = windows_manifest();
        m.windows_x86_64_size = Some(size);
        assert!(m.to_toml().is_err(), "size {size}");
    }
    for version in ["0.86", "0.086.0", "0.86.0+gabc", "0.86.0/../1"] {
        let mut m = windows_manifest();
        m.version = version.into();
        m.linux_aarch64 = None;
        m.linux_aarch64_sha256 = None;
        m.linux_aarch64_size = None;
        m.windows_x86_64 = Some(crate::windows::WindowsTarget::X86_64.asset_name(version));
        assert!(m.to_toml().is_err(), "version {version}");
    }
}

/// The cutter's write is the reader's rule: an entry the selector would refuse is refused and
/// changes nothing, and an accepted one reads back as itself.
#[test]
fn the_cutter_sets_a_windows_entry_only_when_it_reads_back() {
    use crate::windows::{WINDOWS_ZIP_MAX_BYTES, WindowsArtifact, WindowsTarget};
    let digest = "ef".repeat(32);
    let shouting = "EF".repeat(32);
    let good = WindowsArtifact {
        target: WindowsTarget::X86_64,
        name: "aterm-0.86.0-windows-x86_64.zip",
        sha256: &digest,
        size: 90_000_000,
    };
    let mut m = linux_manifest();
    let before = m.clone();
    for bad in [
        WindowsArtifact {
            name: "aterm-0.85.0-windows-x86_64.zip",
            ..good
        },
        WindowsArtifact {
            sha256: &shouting,
            ..good
        },
        WindowsArtifact { size: 63, ..good },
        WindowsArtifact {
            size: WINDOWS_ZIP_MAX_BYTES + 1,
            ..good
        },
    ] {
        assert!(m.set_windows_artifact(&bad).is_err(), "{bad:?}");
        assert_eq!(m, before);
    }
    m.set_windows_artifact(&good).unwrap();
    assert_eq!(
        m.windows_artifact(WindowsTarget::X86_64).unwrap(),
        Some(good)
    );
    assert_eq!(m, windows_manifest());
    assert_eq!(Manifest::parse(&m.to_toml().unwrap()).unwrap(), m);
}

#[test]
fn absent_schema_is_zero_and_unknown_keys_tolerated() {
    let m = Manifest::parse(
        r#"version = "1.0.0"
               build_number = 10
               sha256 = "x"
               dmg = "a.dmg"
               some_future_key = "ignored""#,
    )
    .unwrap();
    assert_eq!(
        m.schema, 0,
        "absent schema must default to 0 (v0.25 semantics)"
    );
    assert_eq!(m.commit, None);
    assert_eq!(m.url, None);
    assert_eq!(
        (m.zip.as_deref(), m.zip_sha256.as_deref()),
        (None, None),
        "a manifest cut before zip staging must still parse, with no zip"
    );
    assert_eq!(m.min_os, None);
    assert_eq!(m.team_id, None);
    assert_eq!(m.pub_date, None);
    assert_eq!(m.min_build, None);
    assert_eq!(m.changelog, None);
}

#[test]
fn rejects_newer_schema() {
    let r = Manifest::parse(
        r#"schema = 99
               version = "9.0.0"
               build_number = 999999
               sha256 = "x"
               dmg = "aterm-9.0.0.dmg""#,
    );
    assert!(r.is_err(), "a future schema must be rejected");
}

#[test]
fn full_manifest_round_trips_and_changelog_is_a_literal_block() {
    let m = full();
    let text = m.to_toml().unwrap();
    assert!(
        text.contains("changelog = '''\n### Added\n"),
        "changelog must be a ''' multiline literal, got:\n{text}"
    );
    assert!(
        !text.contains("with \\\"quotes\\\"") && text.contains("with \"quotes\""),
        "literal block must carry the body unescaped, got:\n{text}"
    );
    assert_eq!(
        Manifest::parse(&text).unwrap(),
        m,
        "byte round-trip must be exact"
    );
}

#[test]
fn absent_optionals_are_omitted_from_emission() {
    let m = Manifest {
        schema: 1,
        version: "0.26".into(),
        build_number: 7,
        commit: None,
        dmg: "a.dmg".into(),
        sha256: "x".into(),
        url: None,
        zip: None,
        zip_sha256: None,
        dmg_x86_64: None,
        dmg_x86_64_sha256: None,
        linux_x86_64: None,
        linux_x86_64_sha256: None,
        linux_x86_64_size: None,
        linux_aarch64: None,
        linux_aarch64_sha256: None,
        linux_aarch64_size: None,
        linux_verifier_x86_64: None,
        linux_verifier_x86_64_sha256: None,
        linux_verifier_x86_64_size: None,
        linux_verifier_aarch64: None,
        linux_verifier_aarch64_sha256: None,
        linux_verifier_aarch64_size: None,
        linux_verifier_interface: None,
        windows_x86_64: None,
        windows_x86_64_sha256: None,
        windows_x86_64_size: None,
        windows_verifier_x86_64: None,
        windows_verifier_x86_64_sha256: None,
        windows_verifier_x86_64_size: None,
        windows_verifier_interface: None,
        min_os: None,
        team_id: None,
        pub_date: None,
        min_build: None,
        machine_id: None,
        roster_seq: None,
        changelog: None,
    };
    let text = m.to_toml().unwrap();
    for absent in [
        "min_build",
        "commit",
        "url",
        "zip",
        "zip_sha256",
        "linux_x86_64",
        "linux_x86_64_size",
        "linux_aarch64",
        "linux_aarch64_size",
        "linux_verifier",
        "windows",
        "dmg_x86_64",
        "dmg_x86_64_sha256",
        "min_os",
        "team_id",
        "pub_date",
        "machine_id",
        "roster_seq",
        "changelog",
    ] {
        assert!(
            !text.contains(absent),
            "absent key `{absent}` leaked into:\n{text}"
        );
    }
    assert_eq!(Manifest::parse(&text).unwrap(), m);
}

#[test]
fn rejects_min_build_above_build_number_on_parse_and_emit() {
    let text = r#"schema = 1
version = "0.55"
build_number = 550
dmg = "aterm-0.55.dmg"
sha256 = "x"
min_build = 551
"#;
    let err = Manifest::parse(text).unwrap_err();
    assert!(
        err.contains("min_build 551") && err.contains("build_number 550"),
        "{err}"
    );

    let mut m = full();
    m.min_build = Some(m.build_number + 1);
    let err = m.to_toml().unwrap_err();
    assert!(err.contains("exceeds its build_number"), "{err}");

    m.min_build = Some(m.build_number);
    assert_eq!(
        Manifest::parse(&m.to_toml().unwrap()).unwrap().min_build,
        Some(m.build_number)
    );
}

#[test]
fn changelog_literal_edge_cases_round_trip_exactly() {
    for body in [
        "no trailing newline",
        "ends with a quote'",
        "",
        "\nleading blank\n",
    ] {
        let mut m = full();
        m.changelog = Some(body.into());
        let text = m.to_toml().unwrap();
        assert_eq!(
            Manifest::parse(&text).unwrap().changelog.as_deref(),
            Some(body),
            "body {body:?} must survive emit→parse byte-exactly; emitted:\n{text}"
        );
    }
}

#[test]
fn to_toml_refuses_unrepresentable_changelog() {
    let mut m = full();
    m.changelog = Some("evil ''' body".into());
    assert!(m.to_toml().is_err(), "''' in the body must refuse to emit");
    m.changelog = Some("cr\r\nlf".into());
    assert!(m.to_toml().is_err(), "raw \\r must refuse to emit");
}
