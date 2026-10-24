// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Provisioning builder for the pinned `fish` shell from source.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::place::{self, Scratch};
use super::{Host, Machine, Spawn, Target};

/// Environment variables stripped from the `fish` provisioning build.
pub(super) const BUILD_ENV_REMOVED: [&str; 31] = [
    "RUSTFLAGS",
    "RUSTDOCFLAGS",
    "CARGO_ENCODED_*",
    "CARGO_BUILD_*",
    "CARGO_TARGET_*",
    "CARGO_PROFILE_*",
    "RUSTC",
    "RUSTDOC",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "CC",
    "CXX",
    "AR",
    "CFLAGS",
    "CXXFLAGS",
    "CPPFLAGS",
    "LDFLAGS",
    "ARFLAGS",
    "CC_*",
    "CXX_*",
    "AR_*",
    "CFLAGS_*",
    "CXXFLAGS_*",
    "TARGET_*",
    "HOST_*",
    "CRATE_CC_NO_DEFAULTS",
    "CPATH",
    "C_INCLUDE_PATH",
    "CPLUS_INCLUDE_PATH",
    "LIBRARY_PATH",
    "MACOSX_DEPLOYMENT_TARGET",
];

/// Returns all environment variables removed from a provisioning build.
pub(super) fn removed() -> Vec<String> {
    BUILD_ENV_REMOVED
        .iter()
        .chain(crate::CHILD_ENV_REMOVED.iter())
        .map(|k| (*k).to_string())
        .collect()
}

/// Builds the `Spawn` command that compiles `manifest` with `targo` into `target`.
pub(super) fn build_spawn(
    targo: &Path,
    manifest: &Path,
    target: &Path,
    cwd: &Path,
    features: &[&str],
) -> Spawn {
    let mut args: Vec<OsString> = [
        "--unverified",
        "build",
        "--release",
        "--locked",
        "--no-default-features",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    if !features.is_empty() {
        args.push("--features".into());
        args.push(features.join(",").into());
    }
    args.push("--manifest-path".into());
    args.push(manifest.as_os_str().to_owned());
    args.push("--target-dir".into());
    args.push(target.as_os_str().to_owned());
    Spawn {
        program: targo.to_path_buf(),
        args,
        cwd: cwd.to_path_buf(),
        set: vec![
            ("FISH_BUILD_DOCS".to_string(), "0".into()),
            ("PCRE2_SYS_STATIC".to_string(), "1".into()),
            (
                "CARGO_TARGET_DIR".to_string(),
                target.as_os_str().to_owned(),
            ),
        ],
        remove: removed(),
    }
}

/// Fetches, verifies, builds, checks, and installs `fish` from `archive` into `t`.
pub(super) fn build(
    archive: &super::rows::Archive,
    t: &Target,
    machine: &Machine,
    host: &dyn Host,
    say: &mut dyn FnMut(&str),
) -> Result<PathBuf, String> {
    let version = t.version.as_str();
    let features: Vec<&str> = t.features.iter().flatten().map(String::as_str).collect();
    let url = archive.url_at(version);
    let scratch = Scratch::new(&machine.tooling, "fish")?;
    let tarball = place::fetch_checked(host, &scratch, &url, archive.sha256, archive.bytes, say)?;
    let plain = place::plain_tar(host, &scratch, &tarball)?;
    let src = place::unpack(
        &scratch,
        &plain,
        atpkg::extract::Container::Tar,
        1,
        place::UNPACKED_CAP,
        "src",
    )?;
    let manifest = src.join("Cargo.toml");
    for need in [&manifest, &src.join("Cargo.lock")] {
        if !need.is_file() {
            return Err(format!("{url} holds no {}", need.display()));
        }
    }
    let targo = host.targo()?;
    let target = scratch.path().join("target.noindex");
    let spawn = build_spawn(&targo, &manifest, &target, scratch.path(), &features);
    say(&format!(
        "building fish {version}: targo --unverified build --release --locked \
         --no-default-features (FISH_BUILD_DOCS=0 PCRE2_SYS_STATIC=1), a few minutes"
    ));
    let scratch = scratch
        .run_build(host, &spawn)
        .map_err(|e| format!("the fish {version} build failed: {e}"))?;
    place::lay_built(
        scratch,
        host,
        t,
        place::Built {
            row: "fish",
            url,
            program: &target.join("release/fish"),
            licence: &src.join("COPYING"),
            answers: &|first| first.ends_with(&format!(" {version}")),
        },
    )
}
