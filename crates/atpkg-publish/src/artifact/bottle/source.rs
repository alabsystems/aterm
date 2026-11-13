// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The GNU producer builds the pinned Git source with a real runtime prefix.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use atpkg::extract::{Container, ExtractOptions, SymlinkPolicy, extract_tree};

use super::upstream::{Inputs, Origin, Request};
use crate::error::{Error, Result};
use crate::triple::{Arch, Os, Triple};

const PREFIX: &str = "/opt/atpkg/git";
const GLIBC_BASELINE: (u32, u32) = (2, 39);

#[derive(serde::Serialize)]
struct BuildOrigin<'a> {
    git_version: &'a str,
    target: &'a str,
    minimum_glibc: String,
    build_glibc: String,
    headers_glibc: String,
    runtime_prefix: bool,
    rust: bool,
    nls: bool,
    source: &'a Origin,
    dependencies: &'a [Origin],
    build_options: &'a [String],
}

fn run(dir: &Path, tool: &str, args: &[String], seconds: u64) -> Result<std::process::Output> {
    let mut command = aterm_uds::spawnfd::command(tool);
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", dir)
        .current_dir(dir)
        .args(args);
    let out = aterm_uds::deadline::output_until(
        &mut command,
        Instant::now() + Duration::from_secs(seconds),
    )
    .map_err(|error| Error::cannot_run(format!("GNU Git source build {tool}: {error}")))?;
    if !out.status.success() {
        return Err(Error::refused(format!(
            "GNU Git source build {tool} failed: {}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(out)
}

fn admitted_glibc(text: &str) -> bool {
    let Some(version) = text.trim().strip_prefix("glibc ") else {
        return false;
    };
    let mut parts = version.split('.');
    let parsed = parts
        .next()
        .and_then(|major| major.parse::<u32>().ok())
        .zip(parts.next().and_then(|minor| minor.parse::<u32>().ok()));
    matches!(parsed, Some(version) if version.0 == 2 && version <= GLIBC_BASELINE)
        && parts.next().is_none()
}

struct Environment {
    build_glibc: String,
    headers_glibc: String,
}

fn target(triple: Triple) -> (&'static str, &'static str) {
    match triple.arch() {
        Arch::Aarch64 => ("aarch64-linux-gnu", "armv8-a"),
        Arch::X86_64 => ("x86_64-linux-gnu", "x86-64"),
    }
}

fn probe(triple: Triple, work: &Path) -> Result<Environment> {
    if !cfg!(target_os = "linux") || triple.os() != Os::Linux {
        return Err(Error::cannot_run(
            "GNU Git source packages are produced on a Linux builder",
        ));
    }
    let host = run(work, "getconf", &["GNU_LIBC_VERSION".into()], 10)?;
    let host = String::from_utf8_lossy(&host.stdout).trim().to_string();
    if !admitted_glibc(&host) {
        return Err(Error::refused(
            "GNU Git must be built against glibc 2.39 or earlier; a newer builder needs a baseline sysroot",
        ));
    }
    let (machine, _) = target(triple);
    let source = work.join("source-compiler-probe.c");
    let binary = work.join("source-compiler-probe");
    std::fs::write(&source, b"#define PCRE2_CODE_UNIT_WIDTH 8\n#include <features.h>\n#include <stdio.h>\n#include <curl/curl.h>\n#include <pcre2.h>\n#include <expat.h>\n#include <zlib.h>\nint main(void) { printf(\"glibc %d.%d\\n\", __GLIBC__, __GLIBC_MINOR__); return 0; }\n")
        .map_err(|error| Error::io("write target SDK probe", &error))?;
    run(
        work,
        &format!("{machine}-gcc"),
        &[
            format!("-I/usr/include/{machine}"),
            source.to_string_lossy().into_owned(),
            "-o".into(),
            binary.to_string_lossy().into_owned(),
            "-lcurl".into(),
            "-lpcre2-8".into(),
            "-lexpat".into(),
            "-lz".into(),
        ],
        30,
    )?;
    crate::artifact::arch::gate(triple, std::slice::from_ref(&binary))?;
    let headers = run(work, &binary.to_string_lossy(), &[], 10)?;
    let headers = String::from_utf8_lossy(&headers.stdout).trim().to_string();
    if !admitted_glibc(&headers) {
        return Err(Error::refused(
            "GNU Git target headers must use glibc 2.39 or earlier",
        ));
    }
    Ok(Environment {
        build_glibc: host,
        headers_glibc: headers.trim_start_matches("glibc ").to_string(),
    })
}

pub(super) fn can_build(triple: Triple) -> Result<()> {
    let work = crate::channel::Scratch::new("gnu-source-capability")?;
    probe(triple, work.path()).map(|_| ())
}

/// Build from the admitted source on GNU, with explicit target compilers and a bounded ABI floor.
pub(super) fn build(request: &Request, inputs: &Inputs, work: &Path) -> Result<PathBuf> {
    let environment = probe(request.triple, work)?;
    if !inputs.source.url.ends_with(".tar.xz") {
        return Err(Error::refused(
            "the pinned GNU Git source must be a .tar.xz archive",
        ));
    }
    run(
        work,
        "xz",
        &[
            "--decompress".into(),
            "--keep".into(),
            "--memlimit-decompress=256MiB".into(),
            "--".into(),
            inputs.archive.to_string_lossy().into_owned(),
        ],
        60,
    )?;
    let tar = inputs.archive.with_extension("");
    if std::fs::metadata(&tar)
        .map_err(|error| Error::io("read decompressed Git source", &error))?
        .len()
        > 512 * 1024 * 1024
    {
        return Err(Error::refused(
            "Git source tar exceeds the 512 MiB source budget",
        ));
    }
    let source = work.join("git-source");
    std::fs::create_dir(&source)
        .map_err(|error| Error::io("create Git source directory", &error))?;
    extract_tree(
        &tar,
        &source,
        Container::Tar,
        512 * 1024 * 1024,
        ExtractOptions {
            strip_components: 1,
            symlinks: SymlinkPolicy::InRoot,
            allow_global_pax: true,
        },
    )
    .map_err(|error| Error::refused(format!("extract verified Git source: {error}")))?;
    let (machine, cpu) = target(request.triple);
    let options = vec![
        format!("prefix={PREFIX}"),
        "sysconfdir=etc".into(),
        "RUNTIME_PREFIX=YesPlease".into(),
        "NO_RUST=YesPlease".into(),
        "NO_GETTEXT=YesPlease".into(),
        "NO_TCLTK=YesPlease".into(),
        "NO_PYTHON=YesPlease".into(),
        "NO_OPENSSL=YesPlease".into(),
        "USE_LIBPCRE2=YesPlease".into(),
        format!("CC={machine}-gcc"),
        format!("AR={machine}-ar"),
        format!("CFLAGS=-O2 -march={cpu}"),
        format!("CURL_CFLAGS=-I/usr/include/{machine}"),
        "CURL_LDFLAGS=-lcurl".into(),
    ];
    let cores = aterm_cores::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
    let jobs = std::env::var("CARGO_BUILD_JOBS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|jobs| *jobs > 0)
        .map_or(cores, |jobs| jobs.min(cores));
    let mut args = vec![format!("-j{jobs}")];
    args.extend(options.clone());
    eprintln!(
        "atpkg-publish: building verified Git {} for {} (glibc <=2.39, runtime prefix)",
        request.version, request.triple
    );
    run(&source, "make", &args, 600)?;
    let install = work.join("source-install");
    args.push(format!("DESTDIR={}", install.display()));
    args.push("install".into());
    run(&source, "make", &args, 300)?;
    let stage = install.join(PREFIX.trim_start_matches('/'));
    super::materialize_symlinks(&stage)?;
    super::git_text::rewrite_prefix(&stage, PREFIX)?;
    for name in ["COPYING", "README.md"] {
        crate::identity::copy_regular_file(&source.join(name), &stage.join(name))
            .map_err(|error| Error::io("preserve Git source license/readme", &error))?;
    }
    let origin = BuildOrigin {
        git_version: &request.version,
        target: request.triple.as_str(),
        minimum_glibc: format!("{}.{}", GLIBC_BASELINE.0, GLIBC_BASELINE.1),
        build_glibc: environment.build_glibc,
        headers_glibc: environment.headers_glibc,
        runtime_prefix: true,
        rust: false,
        nls: false,
        source: &inputs.source,
        dependencies: &inputs.dependencies,
        build_options: &options,
    };
    let bytes = aterm_json::to_vec(&origin)
        .map_err(|error| Error::refused(format!("serialize BUILD-ORIGIN.json: {error}")))?;
    std::fs::write(stage.join("BUILD-ORIGIN.json"), bytes)
        .map_err(|error| Error::io("write BUILD-ORIGIN.json", &error))?;
    Ok(stage)
}

#[cfg(test)]
#[path = "source_tests.rs"]
mod tests;
