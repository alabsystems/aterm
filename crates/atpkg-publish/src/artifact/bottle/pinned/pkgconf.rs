// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Native pkgconf source build and the isolated package-metadata contract.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use aterm_uds::host::upstream::PKGCONF_CONFIGURE_OPTIONS;

use super::Runner;
use crate::error::{Error, Result};

#[derive(serde::Serialize)]
struct BuildOrigin {
    kind: &'static str,
    configure: &'static [&'static str],
    compiler: String,
    compiler_path: String,
    compiler_sha256: String,
    sdk: String,
    cpu_floor: &'static str,
    macos_deployment_target: &'static str,
    source_date_epoch: u64,
    upstream_tests: &'static str,
}

fn step(
    source: &Path,
    work: &Path,
    tool: &Path,
    args: &[String],
    compiler: Option<(&str, &str)>,
    test_search: Option<&Path>,
) -> Result<String> {
    let mut command = aterm_uds::spawnfd::command(tool);
    command
        .env_clear()
        .env("HOME", work)
        .env("PWD", source)
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("LC_ALL", "C")
        .env("SOURCE_DATE_EPOCH", "0")
        .env("MACOSX_DEPLOYMENT_TARGET", "11.0")
        .current_dir(source)
        .args(args);
    if let Some((cc, sdk)) = compiler {
        command
            .env("CC", cc)
            .env("SDKROOT", sdk)
            .env(
                "CFLAGS",
                format!(
                    "-O2 -g0 -mcpu=apple-m1 -isysroot {sdk} -ffile-prefix-map={}=/atpkg/src/pkgconf-3.0.10",
                    source.display()
                ),
            )
            .env("LDFLAGS", format!("-isysroot {sdk}"));
    }
    if let Some(search) = test_search {
        command.env("PKG_CONFIG_LIBDIR", search);
    }
    let output =
        aterm_uds::deadline::output_until(&mut command, Instant::now() + Duration::from_secs(1200))
            .map_err(|error| Error::cannot_run(format!("pkgconf source build: {error}")))?;
    if !output.status.success() {
        return Err(Error::refused(format!(
            "pkgconf source build {} failed: {}\n{}",
            tool.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn settle_sources(root: &Path) -> Result<()> {
    // Equal extraction times retain the release's generated configure/Makefiles
    // without requiring unrelated Autotools regeneration dependencies.
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir)
            .map_err(|error| Error::io("read pkgconf source directory", &error))?
        {
            let entry = entry.map_err(|error| Error::io("read pkgconf source entry", &error))?;
            let kind = entry
                .file_type()
                .map_err(|error| Error::io("read pkgconf source type", &error))?;
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                crate::identity::open_regular_nonblock(&entry.path())
                    .and_then(|(file, _)| file.set_modified(SystemTime::UNIX_EPOCH))
                    .map_err(|error| Error::io("settle pkgconf source time", &error))?;
            }
        }
    }
    Ok(())
}

pub(super) fn source_decoder() -> Result<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .filter(|directory| directory.is_absolute())
        .find_map(|directory| {
            let path = directory.join("xz");
            atpkg::toolchain::is_executable_file(&path)
                .then(|| std::fs::canonicalize(path).ok())
                .flatten()
        })
        .ok_or_else(|| {
            Error::cannot_run(
                "pkgconf source preparation needs xz from atpkg's native brew compatibility",
            )
        })
}

pub(super) fn unpack_source(archive: &Path, work: &Path, max_bytes: u64) -> Result<PathBuf> {
    let decoder = source_decoder()?;
    step(
        work,
        work,
        &decoder,
        &[
            "--decompress".into(),
            "--keep".into(),
            "--memlimit-decompress=256MiB".into(),
            "--".into(),
            archive.to_string_lossy().into_owned(),
        ],
        None,
        None,
    )?;
    let tar = archive.with_extension("");
    let (_, metadata) = crate::identity::open_regular_nonblock(&tar)
        .map_err(|error| Error::io("read decoded pkgconf source tar", &error))?;
    if metadata.len() > max_bytes {
        return Err(Error::refused(
            "pkgconf source tar exceeds its recipe's extraction bound",
        ));
    }
    Ok(tar)
}

pub(super) fn build(source: &Path, work: &Path) -> Result<aterm_json::Value> {
    settle_sources(source)?;
    let xcrun = Path::new("/usr/bin/xcrun");
    let cc = step(
        source,
        work,
        xcrun,
        &["--find".into(), "clang".into()],
        None,
        None,
    )?;
    let sdk = step(
        source,
        work,
        xcrun,
        &["--sdk".into(), "macosx".into(), "--show-sdk-path".into()],
        None,
        None,
    )?;
    let (cc, sdk) = (cc.trim(), sdk.trim());
    if !Path::new(cc).is_absolute()
        || !Path::new(sdk).is_absolute()
        || !Path::new(sdk).join("usr/lib/libSystem.tbd").is_file()
    {
        return Err(Error::refused(
            "pkgconf needs the native compiler and macOS SDK",
        ));
    }
    let compiler_sha256 = aterm_update_core::sha256_file(Path::new(cc)).map_err(Error::refused)?;
    let compiler = step(
        source,
        work,
        Path::new(cc),
        &["--version".into()],
        None,
        None,
    )?;
    let options: Vec<String> = std::iter::once("./configure".into())
        .chain(
            PKGCONF_CONFIGURE_OPTIONS
                .iter()
                .map(|option| (*option).into()),
        )
        .collect();
    let configured = step(
        source,
        work,
        Path::new("/bin/sh"),
        &options,
        Some((cc, sdk)),
        None,
    )?;
    if configured.contains("unrecognized options") {
        return Err(Error::refused(
            "pkgconf did not recognize its native build options",
        ));
    }
    let cores = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
    let jobs = std::env::var("CARGO_BUILD_JOBS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|jobs| *jobs > 0)
        .map_or(cores, |jobs| jobs.min(cores));
    let make = Path::new("/usr/bin/make");
    step(
        source,
        work,
        make,
        &[format!("-j{jobs}")],
        Some((cc, sdk)),
        None,
    )?;
    let search = work.join("upstream-test-search");
    std::fs::create_dir(&search)
        .map_err(|error| Error::io("create pkgconf test search", &error))?;
    // The unmodified upstream dir-list test needs one registered directory.
    // This override belongs to its tests alone; shipped defaults stay empty.
    step(
        source,
        work,
        make,
        &[format!("-j{jobs}"), "check".into()],
        Some((cc, sdk)),
        Some(&search),
    )?;
    if aterm_update_core::sha256_file(Path::new(cc)).map_err(Error::refused)? != compiler_sha256 {
        return Err(Error::refused("pkgconf compiler changed during the build"));
    }
    aterm_json::to_value(BuildOrigin {
        kind: "pkgconf-static",
        configure: PKGCONF_CONFIGURE_OPTIONS,
        compiler: compiler.trim().into(),
        compiler_path: cc.into(),
        compiler_sha256,
        sdk: sdk.into(),
        cpu_floor: "apple-m1",
        macos_deployment_target: "11.0",
        source_date_epoch: 0,
        upstream_tests: "make check",
    })
    .map_err(|error| Error::refused(format!("encode pkgconf build provenance: {error}")))
}

fn empty_search(text: &str) -> bool {
    [
        "DefaultSearchPaths",
        "SystemIncludePaths",
        "SystemLibraryPaths",
    ]
    .iter()
    .all(|field| {
        let prefix = format!("{field}:");
        let values: Vec<_> = text
            .lines()
            .filter_map(|line| line.strip_prefix(&prefix))
            .collect();
        matches!(values.as_slice(), [value] if matches!(value.trim(), "" | "(null)"))
    })
}

pub(super) fn qualify(
    program: &Path,
    version: &str,
    work: &Path,
    runner: &Runner<'_>,
) -> Result<()> {
    let answer = runner(program, &["--version"], work)?;
    if !answer.status.success() || String::from_utf8_lossy(&answer.stdout).trim() != version {
        return Err(Error::refused(
            "pkgconf executable differs from its pinned version",
        ));
    }
    let personality = runner(program, &["--dump-personality"], work)?;
    if !personality.status.success() || !empty_search(&String::from_utf8_lossy(&personality.stdout))
    {
        return Err(Error::refused(
            "pkgconf must have no implicit package, include or library search paths",
        ));
    }
    if runner(program, &["--exists", "atpkg-nonexistent-package"], work)?
        .status
        .code()
        != Some(1)
    {
        return Err(Error::refused("pkgconf did not refuse an absent package"));
    }
    let fixture = work.join("pkgconf fixtures with spaces");
    std::fs::create_dir_all(&fixture)
        .map_err(|error| Error::io("create pkgconf fixtures", &error))?;
    std::fs::write(fixture.join("atpkg-control.pc"), "Name: atpkg-control\nDescription: managed package control\nVersion: 1.2.3\nLibs: -latpkg_control\nCflags: -DATPKG_CONTROL=1\n")
        .map_err(|error| Error::io("write pkgconf positive fixture", &error))?;
    std::fs::write(fixture.join("atpkg-broken.pc"), "Name: atpkg-broken\nDescription: missing dependency control\nVersion: 1.0\nRequires: atpkg-nonexistent-package\n")
        .map_err(|error| Error::io("write pkgconf negative fixture", &error))?;
    let path = format!("--with-path={}", fixture.display());
    for (flag, expected) in [
        ("--modversion", "1.2.3"),
        ("--cflags", "-DATPKG_CONTROL=1"),
        ("--libs", "-latpkg_control"),
    ] {
        let output = runner(program, &[&path, flag, "atpkg-control"], work)?;
        if !output.status.success() || String::from_utf8_lossy(&output.stdout).trim() != expected {
            return Err(Error::refused(format!(
                "pkgconf {flag} did not resolve the explicit control"
            )));
        }
    }
    if runner(program, &[&path, "--exists", "atpkg-broken"], work)?
        .status
        .code()
        != Some(1)
    {
        return Err(Error::refused(
            "pkgconf accepted a package with a missing required dependency",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::empty_search;

    #[test]
    fn implicit_search_is_empty_only_when_every_field_is_present_once_and_empty() {
        let empty = "DefaultSearchPaths: \nSystemIncludePaths: (null)\nSystemLibraryPaths: \n";
        assert!(empty_search(empty));
        assert!(!empty_search(""));
        assert!(!empty_search(&empty.replace(
            "DefaultSearchPaths: ",
            "DefaultSearchPaths: /opt/homebrew/lib/pkgconfig"
        )));
        assert!(!empty_search(&empty.replace(
            "SystemIncludePaths: (null)",
            "SystemIncludePaths: /usr/include"
        )));
        assert!(!empty_search(&format!("{empty}DefaultSearchPaths: \n")));
    }
}
