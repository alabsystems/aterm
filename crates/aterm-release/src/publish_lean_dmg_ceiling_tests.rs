// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const LEAN_0_67_0: u64 = 31_018_076; // the shipped lean image
const SEEDED_0_63_0: u64 = 1_067_044_990; // a seeded image

#[test]
fn the_v0_63_0_image_is_refused_and_the_lean_one_passes() {
    assert!(validate_lean_dmg_size("0.67.0", LEAN_0_67_0).is_ok());
    assert!(
        validate_release_asset_download_size(SEEDED_0_63_0).is_ok(),
        "the client bound alone let v0.63.0 through"
    );
    let err = validate_lean_dmg_size("0.95.0", SEEDED_0_63_0)
        .expect_err("a seeded image must be refused");
    assert!(err.to_string().contains("aterm-0.95.0.dmg"), "{err}");
}

fn sparse_dmg(label: &str, size: u64) -> (aterm_tempfile::TempDir, PathBuf) {
    let dir = crate::test_scratch_dir(label);
    let path = dir.path().join("aterm-test.dmg");
    let file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)
        .expect("create sparse test DMG");
    file.set_len(size).expect("set sparse test DMG size");
    (dir, path)
}

#[test]
fn the_boundary_is_inclusive() {
    assert!(validate_lean_dmg_size("0.64.0", LEAN_DMG_CEILING_BYTES).is_ok());
    assert!(validate_lean_dmg_size("0.64.0", LEAN_DMG_CEILING_BYTES + 1).is_err());
    assert!(validate_lean_dmg_size("0.64.0", 0).is_ok());
    assert!(validate_release_asset_download_size(0).is_err());
}

#[test]
fn an_oversized_resumed_dmg_is_refused_from_the_actual_file() {
    let (_dir, path) = sparse_dmg("resume", LEAN_DMG_CEILING_BYTES + 1);
    validate_lean_dmg_on_disk(&path, "self-check", Some("abandon"))
        .expect_err("resume must re-check the current dist DMG");
}

#[test]
fn an_oversized_recovered_dmg_is_refused_before_publish() {
    let dir = crate::test_scratch_dir("recovery");
    let path = dir.path().join("aterm-recovered.dmg");
    recover_lean_dmg_to(&path, |destination| {
        let file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(destination)
            .map_err(|error| Error::new(format!("create recovered test DMG: {error}")))?;
        file.set_len(LEAN_DMG_CEILING_BYTES + 1)
            .map_err(|error| Error::new(format!("size recovered test DMG: {error}")))?;
        Ok(VerifiedReleaseAsset {
            id: 7,
            size: 1,
            sha256: "00".repeat(32),
        })
    })
    .expect_err("recovery must reject the retained seeded image");
}

#[test]
fn the_pointer_probe_preserves_the_client_resolvers_three_arms() {
    use aterm_update_core::pointer::{Pointer, PointerError};
    let good = "https://github.com/alabsystems/aterm/releases/download/v0.74.0/aterm-appcast.toml";
    assert_eq!(
        pointer_probe_from(
            "alabsystems/aterm",
            Ok(Pointer {
                tag: "v0.74.0".into(),
                location: good.into()
            })
        )
        .unwrap(),
        PointerProbe::Tag {
            tag: "v0.74.0".into(),
            location: good.into()
        }
    );
    assert_eq!(
        pointer_probe_from(
            "alabsystems/aterm",
            Err(PointerError::NoRelease { url: "x".into() })
        )
        .unwrap(),
        PointerProbe::NoRelease
    );
    assert!(
        pointer_probe_from(
            "alabsystems/aterm",
            Err(PointerError::Transport("curl: (6) DNS".into()))
        )
        .is_err(),
        "a transport failure is an error, never a verdict"
    );
    assert!(pointer_probe_from("alabsystems/aterm", Err(PointerError::UnsafeName)).is_err());
    for refused in [
        PointerError::Refused {
            why: "the redirect's tag is not a release tag this client installs from",
        },
        PointerError::Transient {
            code: 503,
            url: "x".into(),
        },
        PointerError::Unexpected {
            code: 200,
            url: "x".into(),
        },
    ] {
        let text = refused.to_string();
        assert_eq!(
            pointer_probe_from("alabsystems/aterm", Err(refused)).unwrap(),
            PointerProbe::Other(text)
        );
    }
    assert_eq!(
        pointer_probe_from(
            "alabsystems/aterm",
            Err(PointerError::OtherTag {
                tag: "atpkg-index-30".into(),
            })
        )
        .unwrap(),
        PointerProbe::NotAnApp {
            tag: "atpkg-index-30".into()
        }
    );
    for (code, refused) in [
        (
            429,
            PointerError::Transient {
                code: 429,
                url: "x".into(),
            },
        ),
        (
            403,
            PointerError::Unexpected {
                code: 403,
                url: "x".into(),
            },
        ),
    ] {
        let reason = refused.to_string();
        assert_eq!(
            pointer_probe_from("alabsystems/aterm", Err(refused)).unwrap(),
            PointerProbe::Refused { code, reason }
        );
    }
}

#[test]
fn the_cut_asserts_the_manifests_url_bind() {
    let parse = |url: &str| {
        Manifest::parse(&format!(
            "schema = 1\nversion = \"0.74.0\"\nbuild_number = 74\n\
             sha256 = \"{}\"\ndmg = \"aterm-0.74.0.dmg\"\n{url}",
            "ab".repeat(32)
        ))
        .unwrap()
    };
    let bound = parse(
        "url = \"https://github.com/alabsystems/aterm/releases/download/v0.74.0/aterm-0.74.0.dmg\"\n",
    );
    assert_manifest_url_binds("alabsystems/aterm", "v0.74.0", &bound).unwrap();
    for (why, url) in [
        ("absent", ""),
        (
            "another channel",
            "url = \"https://github.com/private/origin/releases/download/v0.74.0/aterm-0.74.0.dmg\"\n",
        ),
        (
            "another tag",
            "url = \"https://github.com/alabsystems/aterm/releases/download/v0.73.0/aterm-0.74.0.dmg\"\n",
        ),
        (
            "another container",
            "url = \"https://github.com/alabsystems/aterm/releases/download/v0.74.0/aterm.dmg\"\n",
        ),
    ] {
        assert_manifest_url_binds("alabsystems/aterm", "v0.74.0", &parse(url)).expect_err(why);
    }
}
