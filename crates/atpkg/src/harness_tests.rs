// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// `tool`'s program file as this host names it (`<tool>.exe` on Windows).
fn exe(tool: &str) -> String {
    format!("{tool}{}", crate::platform::EXE_SUFFIX)
}

#[test]
fn calendar_harness_versions_keep_the_existing_source_stamp_binding() {
    let source = format!("abcdef012{}", "0".repeat(31));
    for version in [
        "0.12.0+gabcdef012",
        "1.20261109.1+gabcdef012",
        "1.20240229.2+gabcdef012",
    ] {
        assert!(
            harness_version_valid(version, Some(source.as_str()), true),
            "{version}"
        );
    }
    for version in [
        "1.20261109.0+gabcdef012",
        "1.20260229.1+gabcdef012",
        "1.20261109.1+g123456789",
        "1.20261109.1",
    ] {
        assert!(
            !harness_version_valid(version, Some(source.as_str()), true),
            "{version}"
        );
    }
    assert!(harness_version_valid("1.20261109.1", None, false));
}

/// THE FIVE HARNESSES ARE HARNESSES AND NOTHING ELSE IS: the vendor programs they wrap,
/// codex (built from source, not a harness here), the toolchain programs and a bare
/// suffix are not.
#[test]
fn a_harness_is_a_word_then_the_suffix() {
    for h in [
        "claude-code-alab",
        "muse-alab",
        "agy-alab",
        "grok-alab",
        "kimi-alab",
    ] {
        assert!(is_harness(h), "{h}");
    }
    for p in [
        "claude",
        "muse",
        "agy",
        "grok",
        "kimi",
        "codex",
        "codex-old",
        "ay",
        "trust",
        "trust-vc",
        "alab",
        "-alab",
        "alab-ay",
        "muse-alab2",
        "",
    ] {
        assert!(!is_harness(p), "{p:?}");
    }
    const _: () = assert!(is_harness("x-alab") && !is_harness("-alab"));
}

/// Grok and Kimi hand their sessions to their harnesses; no other vendor or harness name does.
#[test]
fn grok_and_kimi_hand_their_sessions_to_their_harnesses() {
    for (vendor, harness) in [("grok", "grok-alab"), ("kimi", "kimi-alab")] {
        assert_eq!(twin_harness(vendor), Some((harness, harness)));
        assert!(is_harness(harness));
    }
    for other in [
        "claude",
        "codex",
        "codex-old",
        "agy",
        "muse",
        "grok-alab",
        "kimi-alab",
        "Grok",
        "Kimi",
        "",
    ] {
        assert_eq!(twin_harness(other), None, "{other:?}");
    }
}

/// A complete, active store build of `program` at `build` that puts `tool` on PATH.
fn lay(l: &crate::store::Layout, program: &str, build: u64, tool: &str) -> std::path::PathBuf {
    let dir = l.build_dir(program, build);
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    // Named as the store's shims look for a tool: with the platform's executable suffix.
    let exe = dir
        .join("bin")
        .join(format!("{tool}{}", crate::platform::EXE_SUFFIX));
    std::fs::write(&exe, b"#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    crate::store::mark_build_ready(&dir).unwrap();
    crate::activate::activate_build(l, &dir).unwrap();
    crate::activate::install_shims(l, &dir, &[tool.to_string()], crate::Aliases::Off).unwrap();
    dir
}

/// Make `dir` a codex package naming `version`, an ALab build when `alab`. Its entrypoint is
/// the `bin/codex` [`lay`] put there, with this platform's executable suffix.
fn codex_package(dir: &std::path::Path, version: &str, alab: bool) {
    let suffix = crate::platform::EXE_SUFFIX;
    std::fs::write(
        dir.join(crate::alab::PACKAGE_JSON),
        format!(
            r#"{{"layoutVersion":1,"version":"{version}","target":"aarch64-apple-darwin","entrypoint":"bin/codex{suffix}"}}"#
        ),
    )
    .unwrap();
    if alab {
        std::fs::write(dir.join(crate::alab::ALAB_BUILD_JSON), br#"{"source":"0"}"#).unwrap();
    }
}

/// THE INSTALLED HARNESS PACKAGES, EACH WITH ITS RECORDED VERSION: a harness build reads its
/// `.version` record, codex reads its ALab package's version as codex-alab, a dev-linked
/// harness names its checkout, a harness build staged before the record reads no version
/// (never a guess); a non-harness program and a codex vendor build are not listed (the
/// controls: both are active in the same store).
#[test]
fn installed_lists_each_harness_package_with_its_recorded_version() {
    let l = crate::testdir::layout("atpkg-harness-installed-");
    assert_eq!(installed(&l), Vec::new(), "an empty store lists none");
    let grok = lay(&l, "grok-alab", 17, "grok-alab");
    crate::store::write_version(&grok, "0.3.0").unwrap();
    lay(&l, "muse-alab", 4, "muse-alab");
    let ay = lay(&l, "ay", 9, "ay");
    crate::store::write_version(&ay, "0.9.0").unwrap();
    let codex = lay(&l, "codex", crate::alab::tests::BUILD, "codex");
    codex_package(&codex, "0.164.0+codex.0.130.0", true);
    let checkout = l.prefix.join("kimi-checkout");
    std::fs::create_dir_all(checkout.join("bin")).unwrap();
    std::fs::write(
        checkout.join("bin").join(exe("kimi-alab")),
        b"#!/bin/sh\nexit 0\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(
            checkout.join("bin").join(exe("kimi-alab")),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    crate::linkmode::link(
        &l,
        "kimi-alab",
        &checkout,
        &[std::path::PathBuf::from("bin").join(exe("kimi-alab"))],
    )
    .unwrap();
    let rows = installed(&l);
    /// (harness, program, build, version, linked)
    type Shown<'a> = (&'a str, &'a str, Option<u64>, Option<&'a str>, bool);
    let shown: Vec<Shown<'_>> = rows
        .iter()
        .map(|r| {
            (
                r.harness.as_str(),
                r.program.as_str(),
                r.build,
                r.version.as_deref(),
                r.linked.is_some(),
            )
        })
        .collect();
    assert_eq!(
        shown,
        vec![
            (
                "codex-alab",
                "codex",
                Some(crate::alab::tests::BUILD),
                Some("0.164.0+codex.0.130.0"),
                false
            ),
            ("grok-alab", "grok-alab", Some(17), Some("0.3.0"), false),
            ("kimi-alab", "kimi-alab", None, None, true),
            ("muse-alab", "muse-alab", Some(4), None, false),
        ]
    );
    let kimi = rows.iter().find(|r| r.harness == "kimi-alab").unwrap();
    assert_eq!(
        kimi.linked
            .as_deref()
            .map(std::fs::canonicalize)
            .transpose()
            .unwrap(),
        Some(std::fs::canonicalize(&checkout).unwrap())
    );

    // A codex build that is not an ALab package is the vendor's, not codex-alab.
    let v = crate::testdir::layout("atpkg-harness-installed-vendor-");
    let vendor = lay(&v, "codex", crate::alab::tests::BUILD, "codex");
    codex_package(&vendor, "0.165.0", false);
    assert_eq!(installed(&v), Vec::new(), "a vendor codex is no harness");
    codex_package(&vendor, "0.165.0", true);
    assert_eq!(
        installed(&v).first().map(|r| r.harness.as_str()),
        Some("codex-alab"),
        "the same build with the ALab record is (control)"
    );

    // A dev-linked local harness artifact with a valid local-build.json reports its version.
    std::fs::write(
        checkout.join(LOCAL_BUILD_JSON),
        r#"{"schema":2,"origin":"local-build","program":"kimi-alab","command":"kimi-alab","version":"0.1.0+gaa5d5fe00"}"#,
    )
    .unwrap();
    let kimi_stamped = installed(&l)
        .into_iter()
        .find(|r| r.harness == "kimi-alab")
        .unwrap();
    assert_eq!(kimi_stamped.version.as_deref(), Some("0.1.0+gaa5d5fe00"));
    assert_eq!(
        linked_local_build_version(&l, "kimi-alab").as_deref(),
        Some("0.1.0+gaa5d5fe00")
    );
    assert_eq!(linked_local_build_version(&l, "codex"), None);
    assert_eq!(
        local_build_version_of_exe(&checkout.join("bin").join(exe("kimi-alab")), "kimi-alab")
            .as_deref(),
        Some("0.1.0+gaa5d5fe00")
    );
    assert_eq!(
        local_build_version_of_exe(&checkout.join("bin").join(exe("kimi-alab")), "grok-alab"),
        None,
        "mismatched program is refused"
    );
    let kimi_path = format!("bin/{}", exe("kimi-alab"));
    let bad_size = format!(
        r#"{{"schema":2,"origin":"local-build","program":"kimi-alab","command":"kimi-alab","executables":[{{"path":{kimi_path:?},"size":999,"sha256":"0000000000000000000000000000000000000000000000000000000000000000"}}],"version":"0.1.0+gaa5d5fe00"}}"#
    );
    let bad_sha = format!(
        r#"{{"schema":2,"origin":"local-build","program":"kimi-alab","command":"kimi-alab","executables":[{{"path":{kimi_path:?},"size":17,"sha256":"0000000000000000000000000000000000000000000000000000000000000000"}}],"version":"0.1.0+gaa5d5fe00"}}"#
    );
    for bad in [
        r#"{"schema":2,"origin":"local-build","program":"kimi-alab","command":"kimi-alab","version":"0.1.0"}"#,
        r#"{"schema":2,"origin":"local-build","program":"kimi-alab","command":"kimi-alab","version":"0.1.0 evil"}"#,
        r#"{"schema":2,"origin":"local-build","program":"kimi-alab","command":"kimi-alab","version":"0.1.2+gaa5d5fe00"}"#,
        r#"{"schema":2,"origin":"local-build","program":"kimi-alab","command":"kimi-alab","source":"1111111111111111111111111111111111111111","version":"0.1.0+gaa5d5fe00"}"#,
        r#"{"schema":2,"origin":"local-build","program":"kimi-alab","command":"kimi-alab","source":123,"version":"0.1.0+gaa5d5fe00"}"#,
        r#"{"schema":2,"origin":"local-build","program":"kimi-alab","command":"kimi-alab","source_policy":"dirty","version":"0.1.0+gaa5d5fe00"}"#,
        r#"{"schema":2,"origin":"local-build","program":"kimi-alab","command":"kimi-alab","target":"another-target","version":"0.1.0+gaa5d5fe00"}"#,
        r#"{"schema":2,"origin":"local-build","program":"kimi-alab","command":"kimi-alab","policy":"signed","version":"0.1.0+gaa5d5fe00"}"#,
        r#"{"schema":2,"origin":"local-build","program":"kimi-alab","command":"kimi-alab","profile":"bench","version":"0.1.0+gaa5d5fe00"}"#,
        r#"{"schema":2,"origin":"local-build","program":"kimi-alab","command":"kimi-alab","compiler":"short","version":"0.1.0+gaa5d5fe00"}"#,
        r#"{"schema":2,"origin":"local-build","program":"kimi-alab","command":"kimi-alab","checkout":"relative/path","version":"0.1.0+gaa5d5fe00"}"#,
        r#"{"schema":2,"origin":"local-build","program":"kimi-alab","command":"kimi-alab","build_number":0,"version":"0.1.0+gaa5d5fe00"}"#,
        r#"{"schema":2,"origin":"local-build","program":"kimi-alab","command":"kimi-alab","build_number":"1","version":"0.1.0+gaa5d5fe00"}"#,
        r#"{"schema":2,"origin":"local-build","program":"kimi-alab","command":"other","version":"0.1.0+gaa5d5fe00"}"#,
        r#"{"schema":2,"origin":"local-build","program":"kimi-alab","command":"kimi-alab","exposes":["other"],"version":"0.1.0+gaa5d5fe00"}"#,
        bad_size.as_str(),
        bad_sha.as_str(),
    ] {
        std::fs::write(checkout.join(LOCAL_BUILD_JSON), bad).unwrap();
        assert_eq!(
            local_build_version(&checkout, "kimi-alab"),
            None,
            "malformed version or source mismatch is refused: {bad}"
        );
    }
    let kimi_sha = crate::sha256_file(&checkout.join("bin").join(exe("kimi-alab"))).unwrap();
    std::fs::write(
        checkout.join(LOCAL_BUILD_JSON),
        format!(
            r#"{{"schema":2,"origin":"local-build","program":"kimi-alab","command":"kimi-alab","target":{:?},"policy":"unverified","profile":"release","compiler":"20565c3255adf151f9eb920e89cdf83c38f84abe","executables":[{{"path":{:?},"size":17,"sha256":{:?}}}],"version":"0.1.0+gaa5d5fe00"}}"#,
            crate::cli::current_triple(),
            kimi_path,
            kimi_sha
        ),
    )
    .unwrap();
    assert_eq!(
        local_build_version(&checkout, "kimi-alab").as_deref(),
        Some("0.1.0+gaa5d5fe00")
    );

    #[cfg(unix)]
    {
        let sym_checkout = l.prefix.join("kimi-sym-bin");
        std::fs::create_dir_all(&sym_checkout).unwrap();
        std::os::unix::fs::symlink(checkout.join("bin"), sym_checkout.join("bin")).unwrap();
        std::fs::write(
            sym_checkout.join(LOCAL_BUILD_JSON),
            r#"{"schema":2,"origin":"local-build","program":"kimi-alab","command":"kimi-alab","version":"0.1.0+gaa5d5fe00"}"#,
        )
        .unwrap();
        assert_eq!(
            local_build_version(&sym_checkout, "kimi-alab"),
            None,
            "symlinked bin/ directory in local-build root is refused"
        );
    }

    // claude-code-alab's exposed tool is claude-alab, not claude-code-alab.
    let cc_checkout = l.prefix.join("claude-checkout");
    std::fs::create_dir_all(cc_checkout.join("bin")).unwrap();
    let cc_exe = cc_checkout.join("bin").join(exe("claude-alab"));
    std::fs::write(&cc_exe, b"#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&cc_exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::fs::write(
        cc_checkout.join(LOCAL_BUILD_JSON),
        r#"{"schema":2,"origin":"local-build","program":"claude-code-alab","command":"claude-alab","version":"0.8.0+g123456789"}"#,
    )
    .unwrap();
    crate::linkmode::link(
        &l,
        "claude-code-alab",
        &cc_checkout,
        &[std::path::PathBuf::from("bin").join(exe("claude-alab"))],
    )
    .unwrap();
    assert_eq!(
        linked_local_build_version(&l, "claude-code-alab").as_deref(),
        Some("0.8.0+g123456789")
    );
    let cc_row = installed(&l)
        .into_iter()
        .find(|r| r.harness == "claude-code-alab")
        .unwrap();
    assert_eq!(cc_row.version.as_deref(), Some("0.8.0+g123456789"));

    let cc_store = l.build_dir("claude-code-alab", 5);
    std::fs::create_dir_all(cc_store.join("bin")).unwrap();
    let cc_store_exe = cc_store.join("bin/claude-alab");
    let cc_wrong_exe = cc_store.join("bin/claude-code-alab");
    std::fs::write(&cc_store_exe, b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::write(&cc_wrong_exe, b"#!/bin/sh\nexit 0\n").unwrap();
    crate::store::mark_build_ready(&cc_store).unwrap();
    crate::store::write_version(&cc_store, "0.8.0+g987654321").unwrap();
    assert_eq!(
        store_build_version_of_exe(&l, "claude-code-alab", &cc_store_exe).as_deref(),
        Some("0.8.0+g987654321")
    );
    assert_eq!(
        store_build_version_of_exe(&l, "claude-code-alab", &cc_wrong_exe),
        None,
        "claude-code-alab exposes claude-alab, not claude-code-alab"
    );
    std::fs::write(
        cc_store.with_extension("exposes"),
        b"exposes v1\nclaude-code-alab\n",
    )
    .unwrap();
    assert_eq!(
        store_build_version_of_exe(&l, "claude-code-alab", &cc_wrong_exe),
        None,
        "a forged exposes sidecar cannot admit claude-code-alab as the exposed tool"
    );
    std::fs::write(
        cc_store.with_extension("exposes"),
        b"exposes v1\nclaude-alab\nother\n",
    )
    .unwrap();
    assert_eq!(
        store_build_version_of_exe(&l, "claude-code-alab", &cc_store_exe),
        None,
        "a multi-entry exposes sidecar is refused for a single-tool harness build"
    );
    std::fs::write(
        cc_store.with_extension("exposes"),
        b"exposes v1\nclaude-alab\n",
    )
    .unwrap();
    assert_eq!(
        store_build_version_of_exe(&l, "claude-code-alab", &cc_store_exe).as_deref(),
        Some("0.8.0+g987654321")
    );
    std::fs::write(cc_checkout.join("bin/claude-code-alab"), b"#!/bin/sh\n").unwrap();
    std::fs::write(
        cc_checkout.join(LOCAL_BUILD_JSON),
        r#"{"schema":2,"origin":"local-build","program":"claude-code-alab","command":"claude-code-alab","version":"0.8.0+g123456789"}"#,
    )
    .unwrap();
    assert_eq!(
        local_build_version(&cc_checkout, "claude-code-alab"),
        None,
        "claude-code-alab local receipt naming command claude-code-alab is refused"
    );
}
