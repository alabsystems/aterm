// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A fixed harmless Apple binary, retained as private evidence after the remote cleanup.

use super::{
    apple::Tool,
    model::{Artifact, Journal},
};
use crate::{Error, Result};
use std::io::Read as _;
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
use std::path::{Path, PathBuf};
use std::time::Instant;

pub(super) fn archive(dir: &Path, nonce: &str) -> PathBuf {
    dir.join(format!("aterm-probe-{nonce}.zip"))
}

fn app(dir: &Path) -> PathBuf {
    dir.join("aterm verification.app")
}
fn executable(dir: &Path) -> PathBuf {
    app(dir).join("Contents/MacOS/probe")
}
fn marker(nonce: &str) -> String {
    format!("aterm release capability probe {nonce}")
}

fn certificate_command(prefix: &Path, bundle: &Path) -> aterm_uds::spawnfd::Command {
    let mut option = std::ffi::OsString::from("--extract-certificates=");
    option.push(prefix.as_os_str());
    let mut command = aterm_uds::spawnfd::command("/usr/bin/codesign");
    command.arg("--display").arg(option).arg(bundle);
    command
}
fn plist(nonce: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict><key>CFBundleIdentifier</key><string>com.alabsystems.aterm.verification.{nonce}</string><key>CFBundleName</key><string>aterm verification</string><key>CFBundleExecutable</key><string>probe</string><key>CFBundlePackageType</key><string>APPL</string><key>CFBundleVersion</key><string>1</string><key>CFBundleShortVersionString</key><string>1.0</string></dict></plist>\n"
    )
}

fn private_dir(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(path)
                .map_err(|_| Error::new("cannot create probe artifact directory"))?;
            aterm_durable::durable_dir(path.parent().unwrap())?;
        }
        Err(_) => return Err(Error::new("probe artifact directory is unavailable")),
        Ok(_) => {}
    }
    aterm_uds::private_file::check_private_dir(path)
        .map_err(|_| Error::new("probe artifact directory is not private and owned"))
}

pub(super) fn read(path: &Path) -> Result<Vec<u8>> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::new("probe artifact has no parent"))?;
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| Error::new("invalid probe artifact name"))?;
    let (file, meta) = aterm_uds::private_file::open_private(parent, name)
        .map_err(|_| Error::new("probe artifact is not an owned private regular file"))?;
    if meta.nlink() != 1 || meta.len() > 1024 * 1024 {
        return Err(Error::new("probe artifact has unsafe links or size"));
    }
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::new("cannot read probe artifact"))?;
    if bytes.len() > 1024 * 1024 {
        return Err(Error::new("probe artifact exceeds its bound"));
    }
    Ok(bytes)
}

fn hash(bytes: &[u8]) -> String {
    aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(bytes))
}

fn run(
    tool: &dyn Tool,
    command: &mut aterm_uds::spawnfd::Command,
    deadline: Instant,
) -> Result<std::process::Output> {
    let output = tool.run(command, deadline)?;
    if !output.status.success() {
        return Err(Error::new(format!(
            "probe {} exited unsuccessfully ({})",
            super::apple::operation(command),
            output.status
        )));
    }
    Ok(output)
}

fn source_bytes() -> Result<Vec<u8>> {
    let bytes = std::fs::read("/bin/echo")
        .map_err(|_| Error::new("fixed Apple probe binary is unavailable"))?;
    if bytes.len() > 1024 * 1024 {
        return Err(Error::new("fixed Apple probe binary exceeds its bound"));
    }
    Ok(bytes)
}

/// Prepare a reproducible ad-hoc signature before the final executable path exists.
/// This staging path is never executed; only the final copied bytes are executed.
fn prepared_bytes(tool: &dyn Tool, deadline: Instant) -> Result<Vec<u8>> {
    let scratch = aterm_tempfile::tempdir()
        .map_err(|_| Error::new("cannot stage the fixed probe payload"))?;
    let path = scratch.path().join("probe");
    crate::fsx::write_new(&path, &source_bytes()?, 0o700)?;
    let mut command = aterm_uds::spawnfd::command("/usr/bin/codesign");
    command
        .args([
            "--force",
            "--sign",
            "-",
            "--identifier",
            "com.alabsystems.aterm.verification.payload",
            "--timestamp=none",
        ])
        .arg(&path);
    run(tool, &mut command, deadline)?;
    read(&path)
}

fn execute(tool: &dyn Tool, dir: &Path, nonce: &str, deadline: Instant) -> Result<()> {
    let mut command = aterm_uds::spawnfd::command(executable(dir));
    let expected = marker(nonce);
    command.arg(&expected);
    let output = run(tool, &mut command, deadline)?;
    if output.stdout != format!("{expected}\n").as_bytes() {
        return Err(Error::new(
            "probe artifact did not prove execution from its own path",
        ));
    }
    Ok(())
}

pub(super) fn signature(
    tool: &dyn Tool,
    dir: &Path,
    nonce: &str,
    identity: &str,
    deadline: Instant,
) -> Result<()> {
    if read(&app(dir).join("Contents/Info.plist"))? != plist(nonce).as_bytes() {
        return Err(Error::new("probe bundle metadata changed"));
    }
    let mut verify = aterm_uds::spawnfd::command("/usr/bin/codesign");
    verify
        .args(["--verify", "--deep", "--strict"])
        .arg(app(dir));
    run(tool, &mut verify, deadline)?;
    let mut describe = aterm_uds::spawnfd::command("/usr/bin/codesign");
    describe.args(["--display", "--verbose=4"]).arg(app(dir));
    let output = run(tool, &mut describe, deadline)?;
    let text = std::str::from_utf8(&output.stderr)
        .map_err(|_| Error::new("probe signature metadata is malformed"))?;
    let team = format!("TeamIdentifier={}", aterm_update_core::pins::APPLE_TEAM_ID);
    if !text.lines().any(|line| line == team)
        || !text
            .lines()
            .any(|line| line.starts_with("Authority=Developer ID Application:"))
        || !text.lines().any(|line| line.starts_with("Timestamp="))
        || !text
            .lines()
            .any(|line| line.starts_with("CodeDirectory ") && line.contains("(runtime)"))
    {
        return Err(Error::new(
            "probe lacks the expected timestamped Developer ID runtime signature",
        ));
    }
    // Extract into a fresh private directory so codesign cannot overwrite any evidence.
    let certs = aterm_tempfile::tempdir()
        .map_err(|_| Error::new("cannot stage probe certificate metadata"))?;
    let prefix = certs.path().join("certificate");
    let mut extract = certificate_command(&prefix, &app(dir));
    run(tool, &mut extract, deadline)?;
    let cert = std::fs::read(certs.path().join("certificate0"))
        .map_err(|_| Error::new("cannot read public probe certificate"))?;
    let actual = aterm_codec::hex::encode_lower(
        ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, &cert).as_ref(),
    );
    if !actual.eq_ignore_ascii_case(identity) {
        return Err(Error::new("probe was signed by a different certificate"));
    }
    Ok(())
}

pub(super) fn prepare(
    tool: &dyn Tool,
    dir: &Path,
    journal: &Journal,
    identity: &str,
    keychain: &Path,
    deadline: Instant,
) -> Result<Artifact> {
    for path in [
        app(dir),
        app(dir).join("Contents"),
        app(dir).join("Contents/MacOS"),
    ] {
        private_dir(&path)?;
    }
    let info = app(dir).join("Contents/Info.plist");
    let expected = plist(&journal.nonce);
    if !crate::fsx::write_new(&info, expected.as_bytes(), 0o600)?
        && read(&info)? != expected.as_bytes()
    {
        return Err(Error::new("probe bundle contains foreign metadata"));
    }
    let binary = executable(dir);
    let prepared = prepared_bytes(tool, deadline)?;
    let mut already_signed = false;
    if matches!(crate::fsx::kind(&binary)?, crate::fsx::Kind::Missing) {
        if !crate::fsx::write_new(&binary, &prepared, 0o700)? {
            return Err(Error::new(
                "probe executable appeared before its reservation",
            ));
        }
        aterm_durable::durable_dir(binary.parent().unwrap())?;
    } else {
        // An existing path must be our prepared payload or our completed signature.
        // No executable is ever unlinked here, even after a failed first execution.
        let bytes = read(&binary)?;
        if bytes == source_bytes()? {
            return Err(Error::new(
                "unprepared Apple payload is retained; use this transaction's cleanup",
            ));
        }
        if bytes != prepared {
            signature(tool, dir, &journal.nonce, identity, deadline)?;
            already_signed = true;
        }
    }
    layout(dir)?;
    execute(tool, dir, &journal.nonce, deadline)?;
    if !already_signed {
        let mut sign = aterm_uds::spawnfd::command("/usr/bin/codesign");
        sign.args([
            "--force",
            "--options",
            "runtime",
            "--timestamp",
            "--keychain",
        ])
        .arg(keychain)
        .args(["--sign", identity])
        .arg(app(dir));
        run(tool, &mut sign, deadline)?;
    }
    layout(dir)?;
    signature(tool, dir, &journal.nonce, identity, deadline)?;
    execute(tool, dir, &journal.nonce, deadline)?;
    let archive = archive(dir, &journal.nonce);
    reserve_archive(dir, &archive, &journal.nonce, &hash(&read(&binary)?))?;
    let mut zip = aterm_uds::spawnfd::command("/usr/bin/ditto");
    zip.args(["-c", "-k", "--keepParent"])
        .arg(app(dir))
        .arg(&archive);
    run(tool, &mut zip, deadline)?;
    let bytes = read(&archive)?;
    Ok(Artifact {
        sha256: hash(&bytes),
        executable_sha256: hash(&read(&binary)?),
        bytes: bytes.len() as u64,
        signing_identity: identity.to_owned(),
        team: aterm_update_core::pins::APPLE_TEAM_ID.into(),
        executed: true,
    })
}

/// A failed pre-signing attempt can finish cleanup without touching its unproved image.
pub(super) fn unsigned_retained(dir: &Path, journal: &Journal) -> Result<bool> {
    if !journal.unsigned_cleanup() {
        return Ok(false);
    }
    for path in [
        app(dir),
        app(dir).join("Contents"),
        app(dir).join("Contents/MacOS"),
    ] {
        aterm_uds::private_file::check_private_dir(&path)
            .map_err(|_| Error::new("unsigned probe directory is not private and owned"))?;
    }
    layout(dir)?;
    for path in [
        archive(dir, &journal.nonce),
        dir.join("archive.intent"),
        app(dir).join("Contents/_CodeSignature"),
    ] {
        if !matches!(crate::fsx::kind(&path)?, crate::fsx::Kind::Missing) {
            return Ok(false);
        }
    }
    if read(&app(dir).join("Contents/Info.plist"))? != plist(&journal.nonce).as_bytes() {
        return Ok(false);
    }
    Ok(read(&executable(dir))? == source_bytes()?)
}

fn reserve_archive(dir: &Path, archive: &Path, nonce: &str, executable: &str) -> Result<()> {
    let intent = dir.join("archive.intent");
    let expected = format!("{nonce}\n{executable}\n");
    let prior = crate::fsx::read_private(&intent, 256)?;
    if prior
        .as_ref()
        .is_some_and(|bytes| bytes.as_slice() != expected.as_bytes())
    {
        return Err(Error::new(
            "probe archive reservation names another payload",
        ));
    }
    let existing = match crate::fsx::kind(archive)? {
        crate::fsx::Kind::Missing => None,
        _ => Some(read(archive)?),
    };
    if prior.is_none() && existing.as_ref().is_some_and(|bytes| !bytes.is_empty()) {
        return Err(Error::new("probe archive name contains an unowned file"));
    }
    if prior.is_none() {
        crate::fsx::write_new(&intent, expected.as_bytes(), 0o600)?;
        aterm_durable::durable_dir(dir)?;
    }
    if existing.is_none() && !crate::fsx::write_new(archive, b"", 0o600)? {
        return Err(Error::new("probe archive appeared before its reservation"));
    }
    Ok(())
}

fn layout(dir: &Path) -> Result<()> {
    let bundle = app(dir);
    let mut pending = vec![bundle.clone()];
    while let Some(parent) = pending.pop() {
        for child in
            std::fs::read_dir(&parent).map_err(|_| Error::new("cannot inspect probe bundle"))?
        {
            let child = child.map_err(|_| Error::new("cannot inspect probe bundle entry"))?;
            let path = child.path();
            let relative = path.strip_prefix(&bundle).unwrap();
            let kind = std::fs::symlink_metadata(&path)
                .map_err(|_| Error::new("probe bundle entry is unavailable"))?;
            let directory = matches!(
                relative.to_str(),
                Some("Contents" | "Contents/MacOS" | "Contents/_CodeSignature")
            );
            let file = matches!(
                relative.to_str(),
                Some(
                    "Contents/Info.plist"
                        | "Contents/MacOS/probe"
                        | "Contents/_CodeSignature/CodeResources"
                )
            );
            if kind.uid() != crate::fsx::euid()
                || (directory && !kind.is_dir())
                || (file && (!kind.is_file() || kind.nlink() != 1))
                || (!directory && !file)
            {
                return Err(Error::new(
                    "probe bundle contains an unknown or unowned entry",
                ));
            }
            if directory {
                pending.push(path);
            }
        }
    }
    Ok(())
}

pub(super) fn verify(
    tool: &dyn Tool,
    dir: &Path,
    journal: &Journal,
    deadline: Instant,
) -> Result<()> {
    let artifact = journal
        .artifact
        .as_ref()
        .ok_or_else(|| Error::new("probe has no signed artifact receipt"))?;
    layout(dir)?;
    let bytes = read(&archive(dir, &journal.nonce))?;
    if bytes.len() as u64 != artifact.bytes || hash(&bytes) != artifact.sha256 {
        return Err(Error::new("probe archive changed since its receipt"));
    }
    if hash(&read(&executable(dir))?) != artifact.executable_sha256 {
        return Err(Error::new("probe executable changed since its receipt"));
    }
    signature(
        tool,
        dir,
        &journal.nonce,
        &artifact.signing_identity,
        deadline,
    )?;
    execute(tool, dir, &journal.nonce, deadline)
}

#[cfg(test)]
#[path = "artifact_tests.rs"]
mod tests;
