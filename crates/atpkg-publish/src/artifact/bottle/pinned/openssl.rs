// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Pinned native OpenSSL build and the source signer's Ed25519 contract.

use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use aterm_uds::host::upstream::OPENSSL_CONFIGURE_OPTIONS;

use super::Runner;
use crate::error::{Error, Result};

// OpenSSL retains this logical configuration path in the executable. It must not
// retain a producer's scratch directory. No external config is needed for signing.
const PREFIX: &str = "/opt/atpkg/openssl-signing";

fn build_step(source: &Path, work: &Path, tool: &str, args: &[String]) -> Result<String> {
    let mut command = aterm_uds::spawnfd::command(tool);
    command
        .env_clear()
        .env("HOME", work)
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("SOURCE_DATE_EPOCH", "0")
        .env("MACOSX_DEPLOYMENT_TARGET", "11.0")
        .current_dir(source)
        .args(args);
    let output =
        aterm_uds::deadline::output_until(&mut command, Instant::now() + Duration::from_secs(1200))
            .map_err(|error| Error::cannot_run(format!("OpenSSL source build {tool}: {error}")))?;
    if !output.status.success() {
        return Err(Error::refused(format!(
            "OpenSSL source build {tool} failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn settle_sources(root: &Path) -> Result<()> {
    // The extractor stamps entries as it writes them. Equal times avoid remaking
    // checked-in generated sources solely because of their archive order.
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir)
            .map_err(|error| Error::io("read OpenSSL source directory", &error))?
        {
            let entry = entry.map_err(|error| Error::io("read OpenSSL source entry", &error))?;
            let kind = entry
                .file_type()
                .map_err(|error| Error::io("read OpenSSL source type", &error))?;
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                crate::identity::open_regular_nonblock(&entry.path())
                    .and_then(|(file, _)| file.set_modified(SystemTime::UNIX_EPOCH))
                    .map_err(|error| Error::io("settle OpenSSL source time", &error))?;
            }
        }
    }
    Ok(())
}

pub(super) fn build(source: &Path, work: &Path) -> Result<aterm_json::Value> {
    settle_sources(source)?;
    let compiler = build_step(source, work, "/usr/bin/cc", &["--version".into()])?;
    let perl = build_step(
        source,
        work,
        "/usr/bin/perl",
        &["-e".into(), "print $^V".into()],
    )?;
    let options: Vec<String> = std::iter::once("./Configure".into())
        .chain(
            OPENSSL_CONFIGURE_OPTIONS
                .iter()
                .map(|option| (*option).into()),
        )
        .chain(std::iter::once(format!("--prefix={PREFIX}")))
        .collect();
    build_step(source, work, "/usr/bin/perl", &options)?;
    let cores = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
    let jobs = std::env::var("CARGO_BUILD_JOBS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|jobs| *jobs > 0)
        .map_or(cores, |jobs| jobs.min(cores));
    eprintln!("atpkg-publish: building pinned OpenSSL source with {jobs} jobs");
    build_step(
        source,
        work,
        "/usr/bin/make",
        &[format!("-j{jobs}"), "build_sw".into()],
    )?;
    Ok(aterm_json::json!({
        "kind": "openssl-static", "configure": options,
        "target": "build_sw", "source_date_epoch": 0, "macos_deployment_target": "11.0",
        "compiler": compiler.trim(), "perl": perl.trim()
    }))
}

fn success(program: &Path, args: &[&str], work: &Path, runner: &Runner<'_>) -> Result<()> {
    let result = runner(program, args, work)?;
    if !result.status.success() {
        return Err(Error::refused(format!(
            "OpenSSL {} failed: {}",
            args.first().unwrap_or(&"command"),
            String::from_utf8_lossy(&result.stderr)
        )));
    }
    Ok(())
}

pub(super) fn qualify(
    program: &Path,
    version: &str,
    work: &Path,
    runner: &Runner<'_>,
) -> Result<()> {
    let answer = runner(program, &["version"], work)?;
    if !answer.status.success()
        || !String::from_utf8_lossy(&answer.stdout).starts_with(&format!("OpenSSL {version} "))
    {
        return Err(Error::refused(
            "OpenSSL executable differs from its pinned version",
        ));
    }
    let (key, public) = atpkg_keys::generate().map_err(Error::refused)?;
    aterm_uds::private_file::write_private(work, "fixture-key.der", &key)
        .map_err(|error| Error::io("write generated signing fixture key", &error))?;
    let message = b"ALab source signing package qualification\n";
    let expected = atpkg_keys::sign(&key, message).map_err(Error::refused)?;
    std::fs::write(work.join("message"), message)
        .map_err(|error| Error::io("write signing fixture message", &error))?;
    std::fs::write(work.join("reference.sig"), &expected)
        .map_err(|error| Error::io("write reference signature", &error))?;
    write_public(work, &public)?;
    let verify = [
        "pkeyutl",
        "-verify",
        "-pubin",
        "-inkey",
        "public.pem",
        "-rawin",
        "-in",
        "message",
        "-sigfile",
        "reference.sig",
    ];
    // The actual release path verifies atpkg-keys' signature and signs the same
    // bytes with its DER key. Ed25519 is deterministic, so both signatures agree.
    success(program, &verify, work, runner)?;
    success(
        program,
        &[
            "pkeyutl",
            "-sign",
            "-rawin",
            "-keyform",
            "DER",
            "-inkey",
            "fixture-key.der",
            "-in",
            "message",
            "-out",
            "openssl.sig",
        ],
        work,
        runner,
    )?;
    let actual = crate::identity::read_regular_bytes(&work.join("openssl.sig"), 64)
        .map_err(|error| Error::io("read OpenSSL fixture signature", &error))?;
    if actual.len() != 64 || actual != expected {
        return Err(Error::refused("OpenSSL and atpkg-keys signatures disagree"));
    }
    std::fs::write(work.join("message"), b"tampered signing fixture\n")
        .map_err(|error| Error::io("tamper signing fixture message", &error))?;
    if runner(program, &verify, work)?.status.code() != Some(1) {
        return Err(Error::refused(
            "OpenSSL did not reject the tampered message with exit 1",
        ));
    }
    std::fs::write(work.join("message"), message)
        .map_err(|error| Error::io("restore signing fixture message", &error))?;
    let (_, other_public) = atpkg_keys::generate().map_err(Error::refused)?;
    write_public(work, &other_public)?;
    if runner(program, &verify, work)?.status.code() != Some(1) {
        return Err(Error::refused(
            "OpenSSL did not reject another public key with exit 1",
        ));
    }
    Ok(())
}

fn write_public(work: &Path, public: &str) -> Result<()> {
    let mut der = vec![
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
    ];
    der.extend(
        aterm_codec::base64::decode_strict(public.as_bytes())
            .map_err(|error| Error::refused(error.to_string()))?,
    );
    let pem = aterm_codec::base64::encode(&der)
        .map_err(|_| Error::refused("public key encoding failed"))?;
    std::fs::write(
        work.join("public.pem"),
        format!("-----BEGIN PUBLIC KEY-----\n{pem}\n-----END PUBLIC KEY-----\n"),
    )
    .map_err(|error| Error::io("write signing fixture public key", &error))
}

#[cfg(all(test, unix))]
mod tests {
    use std::cell::Cell;
    use std::os::unix::process::ExitStatusExt;

    use super::*;

    fn probe(fault: &str) -> Result<()> {
        let work = aterm_tempfile::tempdir().expect("private fixture");
        let verifies = Cell::new(0);
        qualify(
            Path::new("/fixture/openssl-signing"),
            "3.5.9",
            work.path(),
            &|_, args, cwd| {
                let mut code = 0;
                let mut stdout = Vec::new();
                if args == ["version"] {
                    stdout = if fault == "version" {
                        b"OpenSSL 3.5.8 fixture\n"
                    } else {
                        b"OpenSSL 3.5.9 fixture\n"
                    }
                    .to_vec();
                } else if args.contains(&"-sign") {
                    let key = std::fs::read(cwd.join("fixture-key.der")).expect("generated key");
                    let message = std::fs::read(cwd.join("message")).expect("message");
                    let mut signature =
                        atpkg_keys::sign(&key, &message).expect("reference signing");
                    if fault == "signature" {
                        signature[0] ^= 1;
                    }
                    if fault == "short" {
                        signature.pop();
                    }
                    std::fs::write(cwd.join("openssl.sig"), signature).expect("signature");
                } else {
                    let n = verifies.get() + 1;
                    verifies.set(n);
                    code = match n {
                        1 => i32::from(fault == "genuine"),
                        2 if fault == "tamper" => 0,
                        3 if fault == "key" => 0,
                        _ if fault == "crash" => {
                            return Ok(std::process::Output {
                                status: std::process::ExitStatus::from_raw(9),
                                stdout,
                                stderr: Vec::new(),
                            });
                        }
                        _ => 1,
                    };
                }
                Ok(std::process::Output {
                    status: std::process::ExitStatus::from_raw(code << 8),
                    stdout,
                    stderr: Vec::new(),
                })
            },
        )
    }

    #[test]
    fn source_signer_requires_matching_signatures_and_normal_negative_verdicts() {
        probe("").expect("signatures agree, invalid inputs are refused");
        for fault in [
            "version",
            "signature",
            "short",
            "genuine",
            "tamper",
            "key",
            "crash",
        ] {
            assert!(probe(fault).is_err(), "{fault} escaped qualification");
        }
    }
}
