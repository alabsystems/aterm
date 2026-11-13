// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read-only source-attestation preflight, separate from Apple's signing tools.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use aterm_uds::spawnfd::{Command, Stdio, command};

use crate::checkout_bin::absolute;

#[derive(Clone, Debug)]
pub(crate) struct Tools {
    openssl: PathBuf,
    keys: PathBuf,
    prerequisites: crate::managed_prerequisites::Tools,
}

impl Tools {
    /// Check the tools and the exact ring the publication engine will use.
    pub(crate) fn probe(
        repo: &Path,
        home: &Path,
        now: i64,
        prepared: Option<&Path>,
    ) -> Result<Self, String> {
        let prerequisites = crate::managed_prerequisites::Tools::acquire(
            &[atpkg::prerequisite::Prerequisite::OpenSslSigning],
            "source-attestation preflight",
        )?;
        Self::probe_held(repo, home, now, prepared, prerequisites)
    }

    pub(crate) fn probe_held(
        repo: &Path,
        home: &Path,
        now: i64,
        prepared: Option<&Path>,
        prerequisites: crate::managed_prerequisites::Tools,
    ) -> Result<Self, String> {
        let openssl = prerequisites.openssl()?.to_path_buf();
        let keys = match std::env::var_os("ATPKG_KEYS").filter(|v| !v.is_empty()) {
            Some(path) => absolute(&path, "ATPKG_KEYS")?,
            None => prepared.map(Path::to_path_buf).or_else(|| {
                let target = std::env::var_os("CARGO_TARGET_DIR")
                    .or_else(|| std::env::var_os("CARGO_BUILD_TARGET_DIR"))
                    .filter(|value| !value.is_empty())
                    .map_or_else(|| repo.join(aterm_toolchain::build_cache::DEFAULT_TARGET), |target| crate::release::anchor(Path::new(&target), repo));
                let helper = target.join("gate/atpkg-keys");
                helper.is_file().then_some(helper)
            }).ok_or("source signing needs this checkout's atpkg-keys helper: run `targo --unverified ship provision --id <machine-id>`, or set ATPKG_KEYS to its absolute executable path")?,
        };
        let tools = Self {
            openssl,
            keys,
            prerequisites,
        };
        tools.check_ring(
            home,
            now,
            aterm_update_core::pins::PAPER_MASTER_PUBKEYS,
            |program, args| tools.run(program, args),
        )?;
        Ok(tools)
    }

    pub(crate) fn apply(&self, cmd: &mut Command) {
        cmd.env("PUBLISH_OPENSSL", &self.openssl)
            .env("ATPKG_KEYS", &self.keys);
    }

    fn run(&self, program: &Path, args: &[OsString]) -> Result<Vec<u8>, String> {
        let mut cmd = command(program);
        self.prerequisites.apply(&mut cmd)?;
        run(cmd, program, args)
    }

    /// Inspect the canonical ring without building helpers, signing, or writing files.
    pub(crate) fn authority(home: &Path, now: i64) -> Result<(), String> {
        Self::ring_authority(home, now, aterm_update_core::pins::PAPER_MASTER_PUBKEYS).map(|_| ())
    }

    fn ring_authority(
        home: &Path,
        now: i64,
        masters: &[&str],
    ) -> Result<(PathBuf, crate::machines::MachineIdentity), String> {
        #[cfg(not(unix))]
        {
            let _ = (home, now, masters);
            Err("current source-signing authority is unavailable on this platform".into())
        }
        #[cfg(unix)]
        {
            use atpkg_keys::provision::{MACHINE_KEY_REL, MACHINE_PUB_REL, RELEASE_ROSTER_REL};

            let key = home.join(MACHINE_KEY_REL);
            let record = home.join(MACHINE_PUB_REL);
            let roster = home.join(RELEASE_ROSTER_REL);
            let metadata = std::fs::symlink_metadata(&key)
                .map_err(|e| format!("source signing needs {}: {e}", key.display()))?;
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err(format!(
                    "source signing key {} is not a regular file",
                    key.display()
                ));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt as _;
                crate::sign::check_credentials_perms(
                    metadata.uid(),
                    metadata.mode(),
                    crate::sign::current_uid()?,
                    &key,
                )?;
            }
            atpkg_keys::provision::refuse_pending_roster_transaction(
                roster.to_str().ok_or("release roster path is not UTF-8")?,
            )?;
            let identity = crate::machines::MachineIdentity::read(&record)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("source signing needs {}", record.display()))?;
            let current = crate::machines::current_roster::resolve(
                home,
                &[&roster],
                masters,
                Ok(None),
                aterm_secrets::client::current_public_roster,
            )
            .map_err(|error| error.to_string())?;
            let who = crate::machines::authorize_cut(
                masters,
                current.selected.bytes.clone(),
                &current.selected.sig,
                &identity.pubkey,
                now,
            )
            .map_err(|error| error.to_string())?;
            current
                .require_canonical(home, masters)
                .map_err(|error| error.to_string())?;
            current
                .recheck(home, masters)
                .map_err(|error| error.to_string())?;
            if who.machine_id != identity.id {
                return Err("source signing ring identity disagrees with its signed roster".into());
            }
            Ok((key, identity))
        }
    }

    fn check_ring(
        &self,
        home: &Path,
        now: i64,
        masters: &[&str],
        run: impl Fn(&Path, &[OsString]) -> Result<Vec<u8>, String>,
    ) -> Result<(), String> {
        let (key, identity) = Self::ring_authority(home, now, masters)?;
        let derived = run(&self.keys, &["pubkey".into(), key.as_os_str().into()])?;
        if derived.trim_ascii() != identity.pubkey.as_bytes() {
            return Err("source signing helper and ring identity disagree".into());
        }
        let scratch = aterm_tempfile::Builder::new()
            .prefix("aterm-source-sign-")
            .suffix(".noindex")
            .tempdir()
            .map_err(|e| e.to_string())?;
        let message = scratch.path().join("message");
        let signature = scratch.path().join("signature");
        let public = scratch.path().join("public.pem");
        crate::durable::write_fresh(&message, b"aterm source signing preflight v1\n")
            .map_err(|e| e.to_string())?;
        let mut der = vec![
            0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
        ];
        der.extend(
            aterm_codec::base64::decode_strict(identity.pubkey.as_bytes())
                .map_err(|_| "source signing public key is not base64")?,
        );
        let pem = aterm_codec::base64::encode(&der).map_err(|_| "public key encoding failed")?;
        crate::durable::write_fresh(
            &public,
            format!("-----BEGIN PUBLIC KEY-----\n{pem}\n-----END PUBLIC KEY-----\n"),
        )
        .map_err(|e| e.to_string())?;
        run(
            &self.keys,
            &[
                "sign".into(),
                key.as_os_str().into(),
                message.as_os_str().into(),
                signature.as_os_str().into(),
            ],
        )?;
        run(
            &self.openssl,
            &[
                "pkeyutl".into(),
                "-verify".into(),
                "-pubin".into(),
                "-inkey".into(),
                public.as_os_str().into(),
                "-rawin".into(),
                "-in".into(),
                message.as_os_str().into(),
                "-sigfile".into(),
                signature.as_os_str().into(),
            ],
        )?;
        let fallback = scratch.path().join("fallback-signature");
        run(
            &self.openssl,
            &[
                "pkeyutl".into(),
                "-sign".into(),
                "-rawin".into(),
                "-keyform".into(),
                "DER".into(),
                "-inkey".into(),
                key.as_os_str().into(),
                "-in".into(),
                message.as_os_str().into(),
                "-out".into(),
                fallback.as_os_str().into(),
            ],
        )?;
        let read_signature = |path: &Path| {
            let bytes = crate::durable::read_regular_bytes(path, 64)
                .map_err(|e| format!("source signing signature {}: {e}", path.display()))?;
            if bytes.len() != 64 {
                return Err(format!(
                    "source signing signature {} is {} bytes, expected 64",
                    path.display(),
                    bytes.len()
                ));
            }
            Ok(bytes)
        };
        if read_signature(&signature)? != read_signature(&fallback)? {
            return Err("source signing helper and OpenSSL fallback disagree".into());
        }
        Ok(())
    }
}

/// The source signer's helper, `crates/atpkg-keys`, as [`crate::checkout_bin`] builds it.
const HELPER: crate::checkout_bin::Bin = crate::checkout_bin::Bin {
    package: "atpkg-keys",
    env: "ATPKG_KEYS",
    what: "source-signing helper",
};

/// Build the source signer's helper through the selected checkout's Cargo
/// graph. An explicit helper remains authoritative and is never replaced.
pub(crate) fn prepare_helper(
    repo: &Path,
    explicit: Option<&OsStr>,
    env: &aterm_verify::EnvSnapshot,
    build: impl FnOnce(&[OsString], &aterm_verify::NativeBuildPlan) -> Result<Vec<u8>, String>,
) -> Result<PathBuf, String> {
    crate::checkout_bin::prepare(repo, &HELPER, explicit, env, build)
}

#[cfg(test)]
fn built_helper(repo: &Path, output: &[u8]) -> Result<PathBuf, String> {
    crate::checkout_bin::built(repo, &HELPER, output)
}

fn run(mut cmd: Command, program: &Path, args: &[OsString]) -> Result<Vec<u8>, String> {
    cmd.args(args).stdin(Stdio::null());
    let out = aterm_uds::deadline::output_within(&mut cmd, std::time::Duration::from_secs(60))
        .map_err(|e| {
            format!(
                "source signing tool {} could not run: {e}",
                program.display()
            )
        })?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        // A broken signer could print key material; never copy its output to diagnostics.
        Err(format!(
            "source signing tool {} exited {}",
            program.display(),
            out.status
        ))
    }
}

#[cfg(test)]
#[path = "source_signing_tests.rs"]
mod tests;
