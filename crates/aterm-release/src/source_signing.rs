// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read-only source-attestation preflight, separate from Apple's signing tools.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use aterm_uds::spawnfd::{Command, Stdio, command};

#[derive(Clone, Debug)]
pub(crate) struct Tools {
    openssl: PathBuf,
    keys: PathBuf,
}

impl Tools {
    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        Self {
            openssl: "/fixture/openssl".into(),
            keys: "/fixture/keys".into(),
        }
    }

    /// Check the tools and the exact ring the publication engine will use.
    pub(crate) fn probe(
        repo: &Path,
        home: &Path,
        now: i64,
        prepared: Option<&Path>,
    ) -> Result<Self, String> {
        let explicit = std::env::var_os("PUBLISH_OPENSSL");
        let mut candidates = Vec::new();
        if let Some(path) = aterm_uds::host::tool("openssl-signing").find() {
            candidates.push(path);
        }
        if let Some(path) = aterm_uds::host::tool("openssl").find() {
            candidates.push(path);
        }
        candidates.extend([
            PathBuf::from("/opt/homebrew/bin/openssl"),
            PathBuf::from("/usr/local/bin/openssl"),
            PathBuf::from("/opt/homebrew/opt/openssl/bin/openssl"),
            PathBuf::from("/opt/homebrew/opt/openssl@3/bin/openssl"),
        ]);
        let openssl = select_openssl(explicit.as_deref(), &candidates, |path| {
            run(path, &[OsString::from("version")])
        })?;
        let keys = match std::env::var_os("ATPKG_KEYS").filter(|v| !v.is_empty()) {
            Some(path) => absolute(&path, "ATPKG_KEYS")?,
            None => prepared.map(Path::to_path_buf).or_else(|| {
                let target = std::env::var_os("CARGO_TARGET_DIR")
                    .or_else(|| std::env::var_os("CARGO_BUILD_TARGET_DIR"))
                    .filter(|value| !value.is_empty())
                    .map_or_else(|| repo.join("target"), |target| crate::release::anchor(Path::new(&target), repo));
                let helper = target.join("gate/atpkg-keys");
                helper.is_file().then_some(helper)
            }).ok_or("source signing needs this checkout's atpkg-keys helper: run `targo --unverified ship provision --id <machine-id>`, or set ATPKG_KEYS to its absolute executable path")?,
        };
        let tools = Self { openssl, keys };
        tools.check_ring(
            home,
            now,
            aterm_update_core::pins::PAPER_MASTER_PUBKEYS,
            run,
        )?;
        Ok(tools)
    }

    pub(crate) fn apply(&self, cmd: &mut Command) {
        cmd.env("PUBLISH_OPENSSL", &self.openssl)
            .env("ATPKG_KEYS", &self.keys);
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
        let raw = crate::durable::read_regular_bytes(&roster, 64 * 1024)
            .map_err(|e| format!("source signing needs a readable {}: {e}", roster.display()))?;
        let sig_path = roster.with_extension("toml.sig");
        let sig = crate::durable::read_regular_bytes(&sig_path, 64).map_err(|e| {
            format!(
                "source signing needs a readable {}: {e}",
                sig_path.display()
            )
        })?;
        let who = crate::machines::authorize_cut(masters, raw, &sig, &identity.pubkey, now)
            .map_err(|e| e.to_string())?;
        if who.machine_id != identity.id {
            return Err("source signing ring identity disagrees with its signed roster".into());
        }
        Ok((key, identity))
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
        let scratch = aterm_tempfile::tempdir().map_err(|e| e.to_string())?;
        let message = scratch.path().join("message");
        let signature = scratch.path().join("signature");
        let public = scratch.path().join("public.pem");
        std::fs::write(&message, b"aterm source signing preflight v1\n")
            .map_err(|e| e.to_string())?;
        let mut der = vec![
            0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
        ];
        der.extend(
            aterm_codec::base64::decode_strict(identity.pubkey.as_bytes())
                .map_err(|_| "source signing public key is not base64")?,
        );
        let pem = aterm_codec::base64::encode(&der).map_err(|_| "public key encoding failed")?;
        std::fs::write(
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

/// Build the source signer's helper through the selected checkout's Cargo
/// graph. An explicit helper remains authoritative and is never replaced.
pub(crate) fn prepare_helper(
    repo: &Path,
    explicit: Option<&OsStr>,
    build: impl FnOnce(&[OsString]) -> Result<Vec<u8>, String>,
) -> Result<PathBuf, String> {
    if let Some(path) = explicit.filter(|value| !value.is_empty()) {
        return ready_helper(absolute(path, "ATPKG_KEYS")?);
    }
    let mut args: Vec<OsString> = [
        "--unverified",
        "build",
        "--locked",
        "--profile",
        "gate",
        "-p",
        "atpkg-keys",
        "--bin",
        "atpkg-keys",
        "--message-format=json",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    // The selected driver may be the compiler's own targo, rather than the
    // atpkg shim. Give it the same quiet shared cache layout as verification.
    let env = aterm_verify::EnvSnapshot::capture();
    let target = env
        .cargo_target_dir
        .clone()
        .unwrap_or_else(|| repo.join("target"));
    if let Some(layout) = aterm_verify::main_build_dir(&env, repo)
        .or_else(|| aterm_verify::fallback_build_dir(&env, repo, &target))
    {
        args.extend([OsString::from("--config"), layout.into()]);
    }
    args.push("--manifest-path".into());
    args.push(repo.join("Cargo.toml").into_os_string());
    ready_helper(built_helper(repo, &build(&args)?)?)
}

fn ready_helper(path: PathBuf) -> Result<PathBuf, String> {
    if aterm_toolchain::is_executable_file(&path) {
        Ok(path)
    } else {
        Err(format!(
            "source-signing helper {} is not an executable file",
            path.display()
        ))
    }
}

fn built_helper(repo: &Path, output: &[u8]) -> Result<PathBuf, String> {
    let manifest = repo.join("crates/atpkg-keys/Cargo.toml");
    let mut found = std::collections::BTreeSet::new();
    for line in output.split(|byte| *byte == b'\n') {
        let Ok(row) = aterm_json::from_slice::<aterm_json::Value>(line) else {
            continue;
        };
        if row["reason"].as_str() == Some("compiler-artifact")
            && row["target"]["name"].as_str() == Some("atpkg-keys")
            && row["target"]["kind"]
                .as_array()
                .is_some_and(|kinds| kinds.iter().any(|kind| kind.as_str() == Some("bin")))
            && row["manifest_path"].as_str().map(Path::new) == Some(manifest.as_path())
            && let Some(exe) = row["executable"]
                .as_str()
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
        {
            found.insert(exe);
        }
    }
    if found.len() != 1 {
        return Err(
            "the atpkg-keys build did not identify exactly one helper from this checkout".into(),
        );
    }
    found
        .pop_first()
        .ok_or_else(|| "the atpkg-keys build reported no helper".into())
}

fn absolute(value: &OsStr, name: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(format!("{name} must name an absolute executable path"))
    }
}

fn select_openssl(
    explicit: Option<&OsStr>,
    candidates: &[PathBuf],
    version: impl Fn(&Path) -> Result<Vec<u8>, String>,
) -> Result<PathBuf, String> {
    let paths = match explicit.filter(|v| !v.is_empty()) {
        Some(path) => vec![absolute(path, "PUBLISH_OPENSSL")?],
        None => candidates.to_vec(),
    };
    for path in paths {
        if let Ok(bytes) = version(&path) {
            let line = String::from_utf8_lossy(&bytes);
            if line
                .strip_prefix("OpenSSL ")
                .and_then(|s| s.split('.').next())
                .filter(|s| {
                    !s.is_empty() && !s.starts_with('0') && s.bytes().all(|b| b.is_ascii_digit())
                })
                .and_then(|s| s.parse::<u32>().ok())
                .is_some_and(|major| major >= 3)
            {
                return Ok(path);
            }
        }
    }
    Err("source signing needs OpenSSL 3 or newer; run `targo --unverified verify provision --release`, or set PUBLISH_OPENSSL to its absolute executable path (Apple's LibreSSL serves a different signing purpose)".into())
}

fn run(program: &Path, args: &[OsString]) -> Result<Vec<u8>, String> {
    let mut cmd = command(program);
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
mod tests {
    use super::*;

    fn helper_artifact(manifest: &str, executable: &str) -> String {
        aterm_json::json!({
            "reason": "compiler-artifact", "manifest_path": manifest,
            "target": {"name": "atpkg-keys", "kind": ["bin"]},
            "executable": executable,
        })
        .to_string()
    }

    #[test]
    fn helper_build_uses_the_candidate_and_cargos_reported_output() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let repo = dir.path();
        let output = repo.join("custom target/gate/atpkg-keys");
        aterm_verify::standin(&output, "exit 0");
        let manifest = repo.join("crates/atpkg-keys/Cargo.toml");
        let helper = prepare_helper(repo, None, |args| {
            assert!(args.iter().any(|arg| arg == "--locked"));
            assert_eq!(args.last(), Some(&repo.join("Cargo.toml").into_os_string()));
            Ok(helper_artifact(manifest.to_str().unwrap(), output.to_str().unwrap()).into_bytes())
        })
        .unwrap();
        assert_eq!(helper, output);
        let wrong = helper_artifact(
            "/foreign/crates/atpkg-keys/Cargo.toml",
            "/foreign/bin/atpkg-keys",
        );
        assert!(built_helper(repo, wrong.as_bytes()).is_err());
        let relative = helper_artifact(manifest.to_str().unwrap(), "relative/helper");
        assert!(built_helper(repo, relative.as_bytes()).is_err());
        let ambiguous = format!(
            "{}\n{}",
            helper_artifact(manifest.to_str().unwrap(), "/one"),
            helper_artifact(manifest.to_str().unwrap(), "/two")
        );
        assert!(built_helper(repo, ambiguous.as_bytes()).is_err());
        assert!(prepare_helper(repo, None, |_| Err("compiler failed".into())).is_err());
    }

    #[test]
    fn an_explicit_helper_is_not_replaced_by_a_build() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let repo = dir.path();
        let helper = repo.join("chosen/helper");
        aterm_verify::standin(&helper, "exit 0");
        assert_eq!(
            prepare_helper(repo, Some(helper.as_os_str()), |_| panic!("must not build")).unwrap(),
            helper
        );
        assert!(
            prepare_helper(repo, Some(repo.join("missing").as_os_str()), |_| panic!(
                "must not build"
            ))
            .is_err()
        );
        assert!(
            prepare_helper(repo, Some(OsStr::new("relative/helper")), |_| panic!(
                "must not build"
            ))
            .is_err()
        );
    }

    #[test]
    fn openssl_selection_rejects_libressl_and_respects_explicit_choice() {
        let old = PathBuf::from("/system/openssl");
        let new = PathBuf::from("/tools/openssl");
        let candidates = [old.clone(), new.clone()];
        let version = |p: &Path| {
            Ok(if p == old {
                b"LibreSSL 3.3.6".to_vec()
            } else {
                b"OpenSSL 4.0.0".to_vec()
            })
        };
        assert_eq!(select_openssl(None, &candidates, version).unwrap(), new);
        assert!(select_openssl(Some(old.as_os_str()), &candidates, version).is_err());
        assert!(select_openssl(Some(OsStr::new("relative")), &candidates, version).is_err());
        for non_canonical in [&b"OpenSSL +3.0.0"[..], &b"OpenSSL 03.0.0"[..]] {
            assert!(
                select_openssl(None, std::slice::from_ref(&new), |_| Ok(
                    non_canonical.to_vec()
                ))
                .is_err()
            );
        }
    }

    #[test]
    fn missing_ring_refuses_before_running_a_signer() {
        let home = aterm_tempfile::tempdir().unwrap();
        let tools = Tools {
            openssl: "/openssl".into(),
            keys: "/keys".into(),
        };
        assert!(
            tools
                .check_ring(home.path(), 0, &["master"], |_, _| panic!(
                    "no signer may run without a ring"
                ))
                .is_err()
        );
    }

    struct Fixture {
        home: aterm_tempfile::TempDir,
        master: String,
        key: Vec<u8>,
        public: String,
    }

    impl Fixture {
        fn new() -> Self {
            use aterm_update_core::roster::{Machine, Roster};
            use atpkg_keys::provision::{MACHINE_KEY_REL, MACHINE_PUB_REL, RELEASE_ROSTER_REL};
            let home = aterm_tempfile::tempdir().unwrap();
            let (master_key, master) = atpkg_keys::generate().unwrap();
            let (key, public) = atpkg_keys::generate().unwrap();
            let key_path = home.path().join(MACHINE_KEY_REL);
            let roster_path = home.path().join(RELEASE_ROSTER_REL);
            std::fs::create_dir_all(roster_path.parent().unwrap()).unwrap();
            std::fs::write(&key_path, &key).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))
                    .unwrap();
            }
            std::fs::write(
                home.path().join(MACHINE_PUB_REL),
                format!("id = \"test\"\npubkey = \"{public}\"\n"),
            )
            .unwrap();
            let roster = Roster {
                schema: 1,
                roster_seq: 1,
                valid_until: "9999-12-31T00:00:00Z".into(),
                machines: vec![Machine {
                    id: "test".into(),
                    pubkey: public.clone(),
                    added_at: String::new(),
                    not_after: None,
                }],
                revoked: vec![],
            }
            .to_toml()
            .unwrap();
            std::fs::write(&roster_path, &roster).unwrap();
            std::fs::write(
                roster_path.with_extension("toml.sig"),
                atpkg_keys::sign(&master_key, roster.as_bytes()).unwrap(),
            )
            .unwrap();
            Self {
                home,
                master,
                key,
                public,
            }
        }

        fn tools(&self) -> Tools {
            Tools {
                openssl: "/fixture/openssl".into(),
                keys: "/fixture/keys".into(),
            }
        }

        fn invoke(
            &self,
            program: &Path,
            args: &[OsString],
            damage_signature: bool,
        ) -> Result<Vec<u8>, String> {
            match args[0].to_str().unwrap() {
                "pubkey" => {
                    assert_eq!(program, Path::new("/fixture/keys"));
                    Ok(self.public.as_bytes().to_vec())
                }
                "sign" => {
                    assert_eq!(program, Path::new("/fixture/keys"));
                    let message = std::fs::read(&args[2]).unwrap();
                    let mut sig = atpkg_keys::sign(&self.key, &message).unwrap();
                    if damage_signature {
                        sig[0] ^= 1;
                    }
                    std::fs::write(&args[3], sig).unwrap();
                    Ok(vec![])
                }
                "pkeyutl" if args[1] == "-sign" => {
                    assert_eq!(program, Path::new("/fixture/openssl"));
                    let message = std::fs::read(&args[8]).unwrap();
                    std::fs::write(&args[10], atpkg_keys::sign(&self.key, &message).unwrap())
                        .unwrap();
                    Ok(vec![])
                }
                "pkeyutl" => {
                    assert_eq!(program, Path::new("/fixture/openssl"));
                    let pem = std::fs::read_to_string(&args[4]).unwrap();
                    let encoded = pem.lines().nth(1).unwrap();
                    let der = aterm_codec::base64::decode_strict(encoded.as_bytes()).unwrap();
                    let public =
                        aterm_codec::base64::decode_strict(self.public.as_bytes()).unwrap();
                    assert_eq!(&der[12..], public);
                    let message = std::fs::read(&args[7]).unwrap();
                    let signature = std::fs::read(&args[9]).unwrap();
                    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, public)
                        .verify(&message, &signature)
                        .map_err(|_| {
                            "independent verification rejected the signature".to_string()
                        })?;
                    Ok(vec![])
                }
                other => panic!("unexpected invocation: {other}"),
            }
        }
    }

    #[test]
    fn complete_ring_must_sign_and_independently_verify() {
        let f = Fixture::new();
        for damage in [false, true] {
            let result = f
                .tools()
                .check_ring(f.home.path(), 100, &[&f.master], |p, a| {
                    f.invoke(p, a, damage)
                });
            assert_eq!(result.is_ok(), !damage);
        }
        let result = f
            .tools()
            .check_ring(f.home.path(), 100, &[&f.master], |_, _| {
                Ok(b"another key".to_vec())
            });
        assert!(result.unwrap_err().contains("identity disagree"));
    }

    #[test]
    fn missing_or_untrusted_roster_never_runs_the_signer() {
        use atpkg_keys::provision::RELEASE_ROSTER_REL;
        let f = Fixture::new();
        let sig = f
            .home
            .path()
            .join(RELEASE_ROSTER_REL)
            .with_extension("toml.sig");
        std::fs::write(&sig, [0u8; 64]).unwrap();
        assert!(
            f.tools()
                .check_ring(f.home.path(), 100, &[&f.master], |_, _| panic!(
                    "untrusted ring"
                ))
                .is_err()
        );
        std::fs::remove_file(sig).unwrap();
        assert!(
            f.tools()
                .check_ring(f.home.path(), 100, &[&f.master], |_, _| panic!(
                    "incomplete ring"
                ))
                .is_err()
        );
    }

    #[test]
    fn authority_requires_the_publishers_canonical_roster_pair() {
        let f = Fixture::new();
        let canonical = f
            .home
            .path()
            .join(atpkg_keys::provision::RELEASE_ROSTER_REL);
        let legacy = f.home.path().join(".aterm/roster/aterm-machines.toml");
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::rename(&canonical, &legacy).unwrap();
        std::fs::rename(
            canonical.with_extension("toml.sig"),
            legacy.with_extension("toml.sig"),
        )
        .unwrap();
        assert!(Tools::ring_authority(f.home.path(), 100, &[&f.master]).is_err());
        std::fs::rename(&legacy, &canonical).unwrap();
        assert!(Tools::ring_authority(f.home.path(), 100, &[&f.master]).is_err());
        std::fs::rename(
            legacy.with_extension("toml.sig"),
            canonical.with_extension("toml.sig"),
        )
        .unwrap();
        assert!(Tools::ring_authority(f.home.path(), 100, &[&f.master]).is_ok());
    }

    #[test]
    fn checked_tools_are_handed_to_the_publisher() {
        let f = Fixture::new();
        let mut cmd = command("/publisher");
        f.tools().apply(&mut cmd);
        let env: Vec<_> = cmd.get_envs().collect();
        assert!(env.contains(&(
            OsStr::new("PUBLISH_OPENSSL"),
            Some(OsStr::new("/fixture/openssl"))
        )));
        assert!(env.contains(&(OsStr::new("ATPKG_KEYS"), Some(OsStr::new("/fixture/keys")))));
    }
}
