// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fish row (docs/DESIGN-init-system-2026-10-20.md §6.2): upstream's release source
//! tarball, pinned by sha256, built here with Trust into a `*.noindex` target directory
//! under the tooling cache, held to `/usr/lib` and `/System` for every library it loads,
//! and stamped with the tarball's sha256, `fish --version` and the features it names.
//!
//! The build names its features (`--no-default-features`, then the row's list): fish's
//! defaults probe the host (`embed-manpages` runs a `sphinx-build` if one is found,
//! `localize-messages` a `msgfmt`), so the same tag would build another program on
//! another host. `FISH_BUILD_DOCS=0`, `PCRE2_SYS_STATIC=1` and `CARGO_TARGET_DIR` are
//! the variables the child is handed, and every variable that would change what cargo,
//! rustc or the C compiler builds is removed from what it inherits
//! ([`BUILD_ENV_REMOVED`]): the build links PCRE2 from the source pcre2-sys carries,
//! never a dynamic one from Homebrew. A cargo config file the person wrote
//! (`~/.cargo/config.toml`) still applies, as it does to every build under their home.
//!
//! Nothing is redistributed: each machine builds its own copy from upstream's source.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::place::{self, Scratch};
use super::{Host, Machine, Spawn, Target};

/// What the build child is never handed, besides `cargo run`'s words about this process
/// (`crate::CHILD_ENV_REMOVED`): every variable that would change what the pinned source
/// builds into, a name ending in `*` standing for each that starts so
/// ([`super::Spawn::remove`]).
///
/// - the flags that replace every config source: `RUSTFLAGS`, `RUSTDOCFLAGS` and their
///   encoded forms;
/// - cargo's config in the environment (`CARGO_BUILD_*`: rustflags, target, target and
///   build dirs, rustc and its wrappers; `CARGO_TARGET_*`: a triple's rustflags, linker
///   and runner; `CARGO_PROFILE_*`), the build re-setting `CARGO_TARGET_DIR` to its own
///   ([`build_spawn`]);
/// - the compiler a build runs instead of the toolchain's: `RUSTC`, `RUSTDOC` and the
///   two wrappers;
/// - the C compiler and its flags pcre2-sys's build reads (cc's `CC`, `CFLAGS`, …, with
///   their `_<target>`, `TARGET_` and `HOST_` forms), the include and library paths a
///   Homebrew shell exports, and the deployment target, which changes the binary.
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

/// Every variable a provisioning build removes: [`BUILD_ENV_REMOVED`], then `cargo
/// run`'s ([`crate::CHILD_ENV_REMOVED`]).
pub(super) fn removed() -> Vec<String> {
    BUILD_ENV_REMOVED
        .iter()
        .chain(crate::CHILD_ENV_REMOVED.iter())
        .map(|k| (*k).to_string())
        .collect()
}

/// The build of `manifest` (the unpacked tree's `Cargo.toml`) with `targo`, into
/// `target` (a `*.noindex` directory), from `cwd` outside the tree, naming `features`.
/// `target` is `--target-dir` and `CARGO_TARGET_DIR` both: fish's build helper reads the
/// variable when it compiles (`option_env!`) and canonicalizes it, falling back to
/// `<tree>/target`, which does not exist (measured: build.rs:22 panics `NotFound`).
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

/// Fetch, check, unpack, build, hold and stamp the row's source `archive` at `t`'s version
/// and features, then place it in `t`'s directory; the placed `bin/fish`.
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
    let src = place::unpack(&scratch, &plain, atpkg::extract::Container::Tar, 1, "src")?;
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
    host.run(&spawn)
        .map_err(|e| format!("the fish {version} build failed: {e}"))?;
    let built = target.join("release/fish");
    if !built.is_file() {
        return Err(format!("the build left no {}", built.display()));
    }
    let tree = scratch.fresh("tree")?;
    let fish = tree.join("bin/fish");
    std::fs::create_dir(tree.join("bin")).map_err(|e| format!("mkdir bin: {e}"))?;
    std::fs::copy(&built, &fish).map_err(|e| format!("could not copy the build: {e}"))?;
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&fish, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("chmod {}: {e}", fish.display()))?;
    }
    if src.join("COPYING").is_file() {
        std::fs::copy(src.join("COPYING"), tree.join("COPYING"))
            .map_err(|e| format!("could not keep COPYING: {e}"))?;
    }
    let foreign = atpkg::macho::foreign_dylibs(&fish)
        .map_err(|e| format!("the built fish cannot be read: {e}"))?;
    if !foreign.is_empty() {
        return Err(format!(
            "the built fish loads {} from outside /usr/lib and /System: refused (the pin is a \
             fish that runs on any Mac; a dynamic PCRE2 from Homebrew is what PCRE2_SYS_STATIC=1 \
             keeps out)",
            foreign.join(", ")
        ));
    }
    let home = scratch.fresh("home")?;
    let probe = Spawn {
        program: fish.clone(),
        args: vec!["--version".into()],
        cwd: scratch.path().to_path_buf(),
        set: ["HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME"]
            .iter()
            .map(|k| ((*k).to_string(), home.clone().into_os_string()))
            .collect(),
        remove: Vec::new(),
    };
    let (scratch, reports) = scratch
        .run_copied(host, &probe)
        .map_err(|e| format!("the built fish did not answer --version: {e}"))?;
    let reports = reports.trim().to_string();
    if !reports.ends_with(&format!(" {version}")) {
        return Err(format!(
            "the built fish answers `{reports}`, not version {version}"
        ));
    }
    place::write_stamp(&tree, &t.stamp("fish", Some(url), "sha256", Some(reports)))?;
    place::place(&scratch, &tree, &t.dir, &|| super::complete(t))?;
    Ok(t.dir.join(&t.exe))
}
