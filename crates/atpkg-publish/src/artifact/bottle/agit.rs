// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Admission of the distribution installed by the agit source build.

use std::path::Path;
use std::time::Duration;

use aterm_uds::spawnfd::Stdio;

use crate::artifact::arch;
use crate::channel::Scratch;
use crate::error::{Error, Result};
use crate::identity::{MAX_META_BYTES, read_regular_text};
use crate::triple::Triple;

pub(super) const SOURCE: &str = "agit requires its completed fork build; use pack-bottle agit --install-tree without donor inputs";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct BuildOrigin {
    product: String,
    upstream_version: String,
    upstream_commit: String,
    fork_commit: String,
    runtime_prefix: bool,
}

impl BuildOrigin {
    pub(super) fn check(&self, version: &str) -> Result<()> {
        let valid_commit = |s: &str| {
            s.len() == 40
                && s.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
                && s.bytes().any(|c| c != b'0')
        };
        let Some((upstream, revision)) = version.split_once(".agit.") else {
            return Err(Error::refused(
                "agit package version must name the upstream version and agit revision",
            ));
        };
        if self.product != "agit"
            || self.upstream_version != upstream
            || upstream.is_empty()
            || revision.parse::<std::num::NonZeroU32>().is_err()
            || !self.runtime_prefix
            || !valid_commit(&self.upstream_commit)
            || !valid_commit(&self.fork_commit)
            || self.upstream_commit == self.fork_commit
        {
            return Err(Error::refused(
                "agit BUILD-ORIGIN.json must identify this fork release, its upstream and fork commits, and its runtime prefix",
            ));
        }
        Ok(())
    }
}

fn run(program: &Path, cwd: &Path, args: &[&str], stdin: Stdio) -> Result<String> {
    let mut command = aterm_uds::spawnfd::command(program);
    command
        .env_clear()
        .env("HOME", cwd)
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .args(args)
        .current_dir(cwd);
    let out = aterm_uds::deadline::output_within_from(&mut command, stdin, Duration::from_secs(60))
        .map_err(|error| Error::cannot_run(format!("agit qualification {args:?}: {error}")))?;
    if !out.status.success() {
        return Err(Error::refused(format!(
            "agit qualification {args:?} exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    String::from_utf8(out.stdout)
        .map_err(|error| Error::refused(format!("agit qualification output: {error}")))
}

pub(super) const CONTROLS: &[&str] = &[
    "fork-revision",
    "git-identity",
    "relocated-helpers",
    "relocated-templates",
    "commit-and-clone",
    "https-fetch-capability",
];

pub(super) fn check_request(request: &super::BottleRequest) -> Result<()> {
    if !matches!(request.input, super::BottleInput::InstallTree(_))
        || !request.dep_bottles.is_empty()
        || !request.donor_roots.is_empty()
    {
        return Err(Error::usage(SOURCE));
    }
    if super::reference::native_host() != Some(request.triple) {
        return Err(Error::cannot_run(format!(
            "agit qualification requires a native {} builder",
            request.triple
        )));
    }
    Ok(())
}

/// Qualify the actual decoded archive on its target and retain the source record used by the tests.
pub(super) fn qualify(stage: &Path, version: &str, triple: Triple) -> Result<BuildOrigin> {
    if super::reference::native_host() != Some(triple) {
        return Err(Error::cannot_run(format!(
            "agit qualification requires a native {triple} builder"
        )));
    }
    let origin = read_regular_text(&stage.join("BUILD-ORIGIN.json"), MAX_META_BYTES)
        .map_err(|error| Error::io("read agit BUILD-ORIGIN.json", &error))?;
    let origin: BuildOrigin = aterm_json::from_str(&origin)
        .map_err(|error| Error::refused(format!("agit BUILD-ORIGIN.json: {error}")))?;
    origin.check(version)?;
    let license = read_regular_text(&stage.join("COPYING"), MAX_META_BYTES)
        .map_err(|error| Error::io("read agit COPYING", &error))?;
    if !license.contains("GNU GENERAL PUBLIC LICENSE") {
        return Err(Error::refused(
            "agit package must include Git's GPL license",
        ));
    }
    let program = stage.join("bin/agit");
    let core = stage.join("libexec/git-core");
    let private_git = core.join("git");
    arch::gate(
        triple,
        &[
            program.clone(),
            private_git.clone(),
            core.join("git-remote-https"),
        ],
    )?;
    let work = Scratch::new("agit-qualification")?;
    let cwd = work.path();
    let expected_version = format!("git version {version}");
    let expected_commit = format!("built from commit: {}", origin.fork_commit);
    for executable in [&program, &private_git] {
        let build = run(
            executable,
            cwd,
            &["version", "--build-options"],
            Stdio::null(),
        )?;
        let mut lines = build.lines();
        if lines.next() != Some(&expected_version) || !lines.any(|line| line == expected_commit) {
            return Err(Error::refused(format!(
                "{} was not built at the version and fork commit selected for the agit package",
                executable.display()
            )));
        }
    }
    let agit = |args: &[&str]| run(&program, cwd, args, Stdio::null());
    let discovered = agit(&["--exec-path"])?;
    let expected =
        std::fs::canonicalize(&core).map_err(|e| Error::io("read agit helper directory", &e))?;
    if std::fs::canonicalize(discovered.trim()).ok().as_ref() != Some(&expected) {
        return Err(Error::refused(
            "agit did not resolve its packaged helper directory",
        ));
    }
    let templates = stage.join("share/git-core/templates");
    let sample = read_regular_text(&templates.join("info/exclude"), MAX_META_BYTES)
        .map_err(|e| Error::io("read packaged agit template", &e))?;
    // A system Git may ship byte-identical templates. This unique file in the private stage
    // proves which directory init actually consumed. Only the original templates are packed.
    let marker = format!(
        "atpkg-template-{}",
        cwd.file_name().unwrap_or_default().to_string_lossy()
    );
    let marker_path = templates.join(&marker);
    std::fs::File::create_new(&marker_path)
        .map_err(|e| Error::io("create agit template probe", &e))?;
    agit(&["init", "-q", "source"])?;
    std::fs::remove_file(&marker_path)
        .map_err(|e| Error::io("remove staged agit template probe", &e))?;
    let dot_git = cwd.join("source/.git");
    if !dot_git.join(marker).is_file()
        || read_regular_text(&dot_git.join("info/exclude"), MAX_META_BYTES)
            .map_err(|e| Error::io("read initialized agit template", &e))?
            != sample
    {
        return Err(Error::refused(
            "agit init did not consume its packaged templates",
        ));
    }
    std::fs::write(cwd.join("source/probe"), b"agit checkout\n")
        .map_err(|error| Error::io("write agit qualification input", &error))?;
    agit(&["-C", "source", "add", "probe"])?;
    agit(&[
        "-C",
        "source",
        "-c",
        "user.name=Qualification",
        "-c",
        "user.email=qualify@example.invalid",
        "commit",
        "-q",
        "-m",
        "probe",
    ])?;
    agit(&["clone", "--no-local", "source", "checkout"])?;
    if std::fs::read(cwd.join("checkout/probe"))
        .map_err(|error| Error::io("read agit checkout", &error))?
        != b"agit checkout\n"
    {
        return Err(Error::refused(
            "agit clone did not restore the committed contents",
        ));
    }
    // The remote-helper protocol answers capabilities locally, before any connection is made.
    let query = cwd.join("remote-commands");
    std::fs::write(&query, b"capabilities\n\n")
        .map_err(|error| Error::io("write agit remote-helper request", &error))?;
    let stdin = std::fs::File::open(query)
        .map_err(|error| Error::io("open agit remote-helper request", &error))?;
    let capabilities = run(
        &program,
        cwd,
        &[
            "-C",
            "checkout",
            "remote-https",
            "origin",
            "https://example.invalid/repo",
        ],
        stdin.into(),
    )?;
    if !capabilities.lines().any(|line| line == "fetch") {
        return Err(Error::refused("agit HTTPS helper did not advertise fetch"));
    }
    Ok(origin)
}

#[cfg(test)]
#[path = "agit_tests.rs"]
mod tests;
