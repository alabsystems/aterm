// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// NEGATIVE CONTROL: the worker cannot resolve the reserved commit from
/// origin; only the private delta bundle makes that exact object available.
#[test]
fn a_worker_imports_the_unclaimed_reservation_without_a_remote_ref() {
    fn git(repo: &Path, args: &[&str]) -> String {
        let out = command("git")
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-C",
            ])
            .arg(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }
    let dir = scratch();
    let source = dir.path().join("source");
    let worker = dir.path().join("worker");
    fs::create_dir(&source).unwrap();
    git(&source, &["init", "-q"]);
    fs::write(source.join("payload"), "R").unwrap();
    git(&source, &["add", "."]);
    git(&source, &["commit", "-qm", "R"]);
    git(
        &source,
        &[
            "clone",
            "-q",
            source.to_str().unwrap(),
            worker.to_str().unwrap(),
        ],
    );
    fs::write(source.join("payload"), "reserved bytes").unwrap();
    git(&source, &["commit", "-qam", "reserved"]);
    let commit = git(&source, &["rev-parse", "HEAD"]);
    git(
        &source,
        &["update-ref", crate::ledger::RESERVED_REF, &commit],
    );
    assert!(
        !command("git")
            .arg("-C")
            .arg(&worker)
            .args(["cat-file", "-e", &commit])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(worker_source_bundle(&source, &"f".repeat(40)).is_err());
    let (_bundle_dir, bundle) = worker_source_bundle(&source, &commit).unwrap();
    let before = git(
        &worker,
        &["for-each-ref", "--format=%(refname) %(objectname)"],
    );
    git(
        &worker,
        &[
            "fetch",
            "-q",
            bundle.to_str().unwrap(),
            crate::ledger::RESERVED_REF,
        ],
    );
    assert_eq!(git(&worker, &["rev-parse", "FETCH_HEAD"]), commit);
    assert_eq!(
        git(&worker, &["show", &format!("{commit}:payload")]),
        "reserved bytes"
    );
    assert_eq!(
        before,
        git(
            &worker,
            &["for-each-ref", "--format=%(refname) %(objectname)"]
        )
    );
}

fn scratch() -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix("aterm-linux-handoff-")
        .tempdir()
        .unwrap()
}

fn fixture(dir: &Path, target: LinuxTarget) -> Artifact {
    let mut bytes = vec![0u8; 80];
    bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    bytes[16..18].copy_from_slice(&3u16.to_le_bytes());
    let machine: u16 = if target == LinuxTarget::Aarch64 {
        183
    } else {
        62
    };
    bytes[18..20].copy_from_slice(&machine.to_le_bytes());
    bytes[20..24].copy_from_slice(&1u32.to_le_bytes());
    bytes[52..54].copy_from_slice(&64u16.to_le_bytes());
    let asset = target.asset_name("0.100.0");
    fs::write(dir.join(&asset), &bytes).unwrap();
    let artifact = Artifact {
        schema: 1,
        version: "0.100.0".into(),
        build_number: 100,
        commit: "a".repeat(40),
        target: target.triple().into(),
        asset: asset.clone(),
        sha256: sha256_file(&dir.join(&asset)).unwrap(),
        size: bytes.len() as u64,
        source_fingerprint: "b".repeat(64),
        compiler_sha256: "c".repeat(64),
        driver_sha256: "d".repeat(64),
        compiler_library_sha256: "c".repeat(64),
        compiler_provenance_sha256: "e".repeat(64),
        compiler_version: format!("binary: trustc\nhost: {}\ntrust: 0.1.0\n", target.triple()),
    };
    fs::write(
        dir.join(format!("{asset}.handoff.toml")),
        aterm_toml::to_string(&artifact).unwrap(),
    )
    .unwrap();
    artifact
}

/// A synthetic static verifier for `target` beside its receipt: an ELF header and one
/// `PT_LOAD` program header (`interpreter` makes it `PT_INTERP`, a dynamic one).
fn verifier_fixture(dir: &Path, target: LinuxTarget, interpreter: bool) -> VerifierArtifact {
    let mut bytes = vec![0u8; 64 + 56];
    bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    bytes[16..18].copy_from_slice(&2u16.to_le_bytes());
    let machine: u16 = if target == LinuxTarget::Aarch64 {
        183
    } else {
        62
    };
    bytes[18..20].copy_from_slice(&machine.to_le_bytes());
    bytes[20..24].copy_from_slice(&1u32.to_le_bytes());
    bytes[32..40].copy_from_slice(&64u64.to_le_bytes());
    bytes[52..54].copy_from_slice(&64u16.to_le_bytes());
    bytes[54..56].copy_from_slice(&56u16.to_le_bytes());
    bytes[56..58].copy_from_slice(&1u16.to_le_bytes());
    let kind = if interpreter { PT_INTERP } else { 1 };
    bytes[64..68].copy_from_slice(&kind.to_le_bytes());
    let asset = pin::asset_name("0.100.0", target.arch());
    fs::write(dir.join(&asset), &bytes).unwrap();
    let verifier = VerifierArtifact {
        schema: 1,
        version: "0.100.0".into(),
        build_number: 100,
        commit: "a".repeat(40),
        target: target.triple().into(),
        asset: asset.clone(),
        sha256: sha256_file(&dir.join(&asset)).unwrap(),
        size: bytes.len() as u64,
        source_fingerprint: "b".repeat(64),
        compiler_version: format!(
            "rustc 1.99.1\nbinary: rustc\nhost: {}\nrelease: 1.99.1\n",
            target.triple()
        ),
    };
    fs::write(
        dir.join(format!("{asset}.handoff.toml")),
        aterm_toml::to_string(&verifier).unwrap(),
    )
    .unwrap();
    verifier
}

/// `ship pin-verifier` writes install.sh's four lines and the committed record from a
/// cut's receipts, and the pair passes `pin::check`; a record this checkout's ledger
/// does not claim, or of another interface, writes nothing.
#[test]
fn pin_verifier_writes_the_pin_and_its_record_together() {
    let root = scratch();
    let script = format!(
        "#!/bin/bash\n# the installer\n{}rest\n",
        Pin::default().lines()
    );
    fs::create_dir_all(root.path().join("tools")).unwrap();
    fs::create_dir_all(root.path().join("crates/aterm-bootstrap-verify")).unwrap();
    fs::write(root.path().join("tools/install.sh"), &script).unwrap();
    fs::write(
        root.path().join("RELEASES.ledger"),
        "# ledger\n100 0.100.0\n",
    )
    .unwrap();
    let record = pin::Record {
        repo: "alabsystems/aterm".into(),
        tag: "v0.100.0".into(),
        build_number: 100,
        commit: "a".repeat(40),
        interface: pin::INTERFACE,
        verifiers: vec![pin::Receipt {
            arch: "x86_64".into(),
            asset: pin::asset_name("0.100.0", "x86_64"),
            sha256: "c".repeat(64),
            size: 4096,
        }],
    };
    let dist = scratch();
    let receipts = dist.path().join(PIN_RECORD);
    let refused = |record: &pin::Record, why: &str| {
        fs::write(&receipts, record.to_toml()).unwrap();
        let said = pin_verifier(root.path(), &receipts).expect_err(why);
        assert!(said.contains(why), "{why}: {said}");
        assert_eq!(
            fs::read_to_string(root.path().join("tools/install.sh")).unwrap(),
            script,
            "{why}: nothing written"
        );
        assert!(!root.path().join(pin::RECORD).exists(), "{why}");
    };
    refused(
        &pin::Record {
            build_number: 101,
            ..record.clone()
        },
        "records no claim",
    );
    refused(
        &pin::Record {
            interface: pin::INTERFACE + 1,
            ..record.clone()
        },
        "speaks interface",
    );
    fs::write(&receipts, record.to_toml()).unwrap();
    assert_eq!(pin_verifier(root.path(), &receipts), Ok(record.pin()));
    let written = fs::read_to_string(root.path().join("tools/install.sh")).unwrap();
    assert_eq!(
        written,
        format!(
            "#!/bin/bash\n# the installer\n{}rest\n",
            record.pin().lines()
        )
    );
    let committed = fs::read_to_string(root.path().join(pin::RECORD)).unwrap();
    assert_eq!(
        pin::check(&written, Some(&committed), "100 0.100.0\n"),
        Ok(record.pin())
    );
}

/// A worker's static verifier joins the cut beside its executable: imported into the
/// stage, uploaded with the Linux assets, its receipt signed in the appcast, and printed as
/// install.sh's pin, which reads back as it was printed. An architecture with none
/// stays on install.sh's embedded verifier (its digest empty in the pin).
#[test]
fn a_workers_static_verifier_is_staged_and_pinned() {
    let source = scratch();
    let dist = scratch();
    for t in TARGETS {
        fixture(source.path(), t);
    }
    let verifier = verifier_fixture(source.path(), LinuxTarget::Aarch64, false);
    let mut handoff = Handoff::new(source.path(), &[], &[]).unwrap();
    handoff
        .import(dist.path(), "0.100.0", 100, &"a".repeat(40))
        .unwrap();
    assert_eq!(handoff.verifiers, std::slice::from_ref(&verifier));
    handoff
        .verify_staged(dist.path(), "0.100.0", 100, &"a".repeat(40))
        .unwrap();
    verifier
        .verify_file(&dist.path().join(&verifier.asset))
        .unwrap();
    let mut manifest = manifest();
    handoff.stamp(&mut manifest).unwrap();
    assert_eq!(
        manifest_asset_names(&manifest).unwrap(),
        [
            LinuxTarget::X86_64.asset_name("0.100.0"),
            LinuxTarget::Aarch64.asset_name("0.100.0"),
            verifier.asset.clone(),
        ],
        "the appcast signs the verifier beside the executables"
    );
    let commit = "a".repeat(40);
    let record = handoff
        .record("alabsystems/aterm", "0.100.0", 100, &commit)
        .expect("a verifier was frozen");
    assert_eq!(
        signed_verifiers(&manifest).unwrap(),
        Some((record.verifiers.clone(), pin::INTERFACE)),
        "the signed receipts are the record's"
    );
    let text = crate::manifest_out::emit(&manifest).unwrap();
    assert_eq!(
        signed_verifiers(&Manifest::parse(&text).unwrap()).unwrap(),
        Some((record.verifiers.clone(), pin::INTERFACE)),
        "and survive the emitted bytes"
    );
    assert_eq!(record.validate(), Ok(()));
    assert_eq!(pin::Record::parse(&record.to_toml()), Ok(record.clone()));
    assert_eq!(
        record.pin(),
        Pin {
            repo: "alabsystems/aterm".into(),
            tag: "v0.100.0".into(),
            x86_64: String::new(),
            aarch64: verifier.sha256.clone(),
        }
    );
    assert_eq!(
        record.verifiers,
        [pin::Receipt {
            arch: "aarch64".into(),
            asset: verifier.asset.clone(),
            sha256: verifier.sha256.clone(),
            size: verifier.size,
        }]
    );
    assert!(
        Handoff::new(source.path(), &[], &[])
            .unwrap()
            .record("alabsystems/aterm", "0.100.0", 100, &commit)
            .is_none()
    );
    let frozen: Handoff = aterm_toml::from_str(&aterm_toml::to_string(&handoff).unwrap()).unwrap();
    assert_eq!(frozen, handoff);
    let old: Handoff = aterm_toml::from_str(
        aterm_toml::to_string(&handoff)
            .unwrap()
            .split("[[verifiers]]")
            .next()
            .unwrap(),
    )
    .unwrap();
    assert!(
        old.verifiers.is_empty(),
        "a journal from before verifiers reads as none"
    );
    let mut published = crate::channel::required_asset_names("0.100.0", false, false);
    let linux: Vec<String> = handoff
        .artifacts
        .iter()
        .map(|a| a.asset.clone())
        .chain([verifier.asset.clone()])
        .collect();
    published.extend(linux.iter().cloned());
    crate::channel::validate_channel_asset_set(&published, "0.100.0", false, false, &linux)
        .unwrap();
    assert!(crate::channel::publishable_asset_names("0.100.0").contains(&verifier.asset));
    let foreign = vec![pin::asset_name("0.99.0", "aarch64")];
    assert!(
        crate::channel::validate_channel_asset_set(&published, "0.100.0", false, false, &foreign)
            .unwrap_err()
            .to_string()
            .contains("noncanonical")
    );
}

/// Negative controls: a dynamic, tampered, misnamed or mislabelled verifier is refused,
/// and a resumed cut refuses a verifier that appeared, changed or went after it froze.
#[test]
fn a_verifier_that_is_not_the_receipts_static_build_is_refused() {
    let source = scratch();
    let dist = scratch();
    let dynamic = verifier_fixture(source.path(), LinuxTarget::Aarch64, true);
    assert!(
        dynamic
            .verify_file(&source.path().join(&dynamic.asset))
            .unwrap_err()
            .contains("static")
    );
    let verifier = verifier_fixture(source.path(), LinuxTarget::Aarch64, false);
    let mut misnamed = verifier.clone();
    misnamed.asset = pin::asset_name("0.100.0", "x86_64");
    assert!(misnamed.validate("0.100.0", 100, &"a".repeat(40)).is_err());
    let mut unknown = verifier.clone();
    unknown.compiler_version = "rustc 1.99.1\n".into();
    assert!(unknown.validate("0.100.0", 100, &"a".repeat(40)).is_err());
    let mut oversized = verifier.clone();
    oversized.size = pin::MAX_ASSET + 1;
    assert!(oversized.validate("0.100.0", 100, &"a".repeat(40)).is_err());
    let mut foreign = verifier.clone();
    foreign.target = LinuxTarget::X86_64.triple().into();
    assert!(
        foreign
            .verify_file(&source.path().join(&verifier.asset))
            .is_err()
    );
    let mut data = fs::read(source.path().join(&verifier.asset)).unwrap();
    data[100] ^= 1;
    fs::write(source.path().join(&verifier.asset), &data).unwrap();
    assert!(
        verifier
            .verify_file(&source.path().join(&verifier.asset))
            .unwrap_err()
            .contains("hash")
    );
    fixture(source.path(), LinuxTarget::Aarch64);
    let mut handoff =
        Handoff::new(source.path(), &[LinuxTarget::Aarch64.triple().into()], &[]).unwrap();
    assert!(
        handoff
            .import(dist.path(), "0.100.0", 100, &"a".repeat(40))
            .unwrap_err()
            .contains("hash"),
        "a tampered verifier fails the import"
    );
    for file in [
        verifier.asset.clone(),
        format!("{}.handoff.toml", verifier.asset),
    ] {
        fs::remove_file(source.path().join(file)).unwrap();
    }
    handoff
        .import(dist.path(), "0.100.0", 100, &"a".repeat(40))
        .unwrap();
    assert!(handoff.verifiers.is_empty());
    verifier_fixture(source.path(), LinuxTarget::Aarch64, false);
    assert!(
        handoff
            .import(dist.path(), "0.100.0", 100, &"a".repeat(40))
            .unwrap_err()
            .contains("changed"),
        "a verifier that appeared after the cut froze none"
    );
    let mut frozen = handoff.clone();
    frozen.verifiers = vec![verifier.clone()];
    frozen.verifiers[0].sha256 = "f".repeat(64);
    assert!(
        frozen
            .import(dist.path(), "0.100.0", 100, &"a".repeat(40))
            .unwrap_err()
            .contains("changed")
    );
    frozen.verifiers = vec![verifier.clone(), verifier];
    assert!(
        frozen
            .validate_identity("0.100.0", 100, &"a".repeat(40), true)
            .is_err(),
        "two verifiers for one architecture"
    );
}

fn manifest() -> Manifest {
    Manifest::parse(&format!("schema=1\nversion=\"0.100.0\"\nbuild_number=100\ncommit=\"{}\"\ndmg=\"aterm-0.100.0.dmg\"\nsha256=\"{}\"\nteam_id=\"\"\n", "a".repeat(40), "f".repeat(64))).unwrap()
}

#[test]
fn imports_both_architectures_and_binds_signed_manifest_fields() {
    let source = scratch();
    let dist = scratch();
    for t in TARGETS {
        fixture(source.path(), t);
    }
    let mut handoff = Handoff::new(source.path(), &[], &[]).unwrap();
    handoff
        .import(dist.path(), "0.100.0", 100, &"a".repeat(40))
        .unwrap();
    handoff
        .verify_staged(dist.path(), "0.100.0", 100, &"a".repeat(40))
        .unwrap();
    let mut manifest = manifest();
    handoff.stamp(&mut manifest).unwrap();
    assert_eq!(manifest_asset_names(&manifest).unwrap().len(), 2);
    assert_eq!(
        signed_verifiers(&manifest),
        Ok(None),
        "no verifier was frozen"
    );
    verify_manifest_handoff(&manifest, Some(&handoff)).unwrap();
    assert!(verify_manifest_handoff(&manifest, None).is_err());
    manifest.linux_aarch64_sha256 = Some("f".repeat(64));
    assert!(verify_manifest_handoff(&manifest, Some(&handoff)).is_err());
}

/// The signed verifier fields are held to the frozen handoff like the executables'
/// (negative control: a digest, a size or an interface the handoff did not freeze, or a
/// receipt it has none for), and a manifest's receipts are read whole or refused.
#[test]
fn signed_verifier_fields_are_bound_to_the_handoff_and_read_whole() {
    let source = scratch();
    let dist = scratch();
    for t in TARGETS {
        fixture(source.path(), t);
        verifier_fixture(source.path(), t, false);
    }
    let mut handoff = Handoff::new(source.path(), &[], &[]).unwrap();
    handoff
        .import(dist.path(), "0.100.0", 100, &"a".repeat(40))
        .unwrap();
    assert_eq!(handoff.verifiers.len(), 2);
    let mut signed = manifest();
    handoff.stamp(&mut signed).unwrap();
    verify_manifest_handoff(&signed, Some(&handoff)).unwrap();
    let (receipts, interface) = signed_verifiers(&signed).unwrap().unwrap();
    assert_eq!(interface, pin::INTERFACE);
    assert_eq!(
        receipts.iter().map(|r| r.arch.as_str()).collect::<Vec<_>>(),
        pin::ARCHES
    );
    let bent = |f: &dyn Fn(&mut Manifest)| {
        let mut m = signed.clone();
        f(&mut m);
        m
    };
    for (why, m) in [
        (
            "digest",
            bent(&|m| m.linux_verifier_x86_64_sha256 = Some("f".repeat(64))),
        ),
        (
            "size",
            bent(&|m| m.linux_verifier_aarch64_size = Some(4096)),
        ),
        (
            "interface",
            bent(&|m| m.linux_verifier_interface = Some(pin::INTERFACE + 1)),
        ),
    ] {
        assert!(
            verify_manifest_handoff(&m, Some(&handoff)).is_err(),
            "{why}"
        );
    }
    let mut bare = Handoff::new(source.path(), &[], &[]).unwrap();
    bare.import(dist.path(), "0.100.0", 100, &"a".repeat(40))
        .unwrap();
    bare.verifiers.clear();
    assert!(
        verify_manifest_handoff(&signed, Some(&bare)).is_err(),
        "a signed receipt the handoff never froze"
    );
    for (why, m) in [
        ("incomplete", bent(&|m| m.linux_verifier_x86_64_size = None)),
        (
            "canonical",
            bent(&|m| m.linux_verifier_x86_64 = Some(pin::asset_name("0.99.0", "x86_64"))),
        ),
        (
            "lowercase",
            bent(&|m| m.linux_verifier_aarch64_sha256 = Some("F".repeat(64))),
        ),
        (
            "outside",
            bent(&|m| m.linux_verifier_aarch64_size = Some(63)),
        ),
        (
            "outside",
            bent(&|m| m.linux_verifier_aarch64_size = Some(pin::MAX_ASSET + 1)),
        ),
        ("no interface", bent(&|m| m.linux_verifier_interface = None)),
        ("no verifier", {
            let mut m = manifest();
            m.linux_verifier_interface = Some(pin::INTERFACE);
            m
        }),
    ] {
        let said = signed_verifiers(&m).expect_err(why);
        assert!(said.contains(why), "{why}: {said}");
    }
}

#[test]
fn explicit_native_subset_is_honest_and_default_refuses_missing_worker() {
    let source = scratch();
    let dist = scratch();
    fixture(source.path(), LinuxTarget::Aarch64);
    assert!(
        Handoff::new(source.path(), &[], &[])
            .unwrap()
            .import(dist.path(), "0.100.0", 100, &"a".repeat(40))
            .is_err()
    );
    let mut handoff =
        Handoff::new(source.path(), &[LinuxTarget::Aarch64.triple().into()], &[]).unwrap();
    handoff
        .import(dist.path(), "0.100.0", 100, &"a".repeat(40))
        .unwrap();
    let mut manifest = manifest();
    handoff.stamp(&mut manifest).unwrap();
    assert!(manifest.linux_x86_64.is_none());
    assert!(manifest.linux_aarch64.is_some());
    assert!(Handoff::new(source.path(), &["riscv64".into()], &[]).is_err());
    assert!(
        Handoff::new(
            source.path(),
            &[
                LinuxTarget::Aarch64.triple().into(),
                LinuxTarget::Aarch64.triple().into()
            ],
            &[]
        )
        .is_err()
    );
}

#[test]
fn resume_refuses_changed_provenance_and_tampered_staged_bytes() {
    let source = scratch();
    let dist = scratch();
    let mut artifact = fixture(source.path(), LinuxTarget::Aarch64);
    let mut handoff =
        Handoff::new(source.path(), std::slice::from_ref(&artifact.target), &[]).unwrap();
    handoff
        .import(dist.path(), "0.100.0", 100, &"a".repeat(40))
        .unwrap();
    let frozen: Handoff = aterm_toml::from_str(&aterm_toml::to_string(&handoff).unwrap()).unwrap();
    assert_eq!(frozen, handoff);
    artifact.compiler_sha256 = "f".repeat(64);
    fs::write(
        source
            .path()
            .join(format!("{}.handoff.toml", artifact.asset)),
        aterm_toml::to_string(&artifact).unwrap(),
    )
    .unwrap();
    assert!(
        handoff
            .import(dist.path(), "0.100.0", 100, &"a".repeat(40))
            .unwrap_err()
            .contains("changed")
    );
    let mut data = fs::read(dist.path().join(&artifact.asset)).unwrap();
    data[70] ^= 1;
    fs::write(dist.path().join(&artifact.asset), data).unwrap();
    assert!(
        frozen
            .verify_staged(dist.path(), "0.100.0", 100, &"a".repeat(40))
            .unwrap_err()
            .contains("hash")
    );
}

#[test]
fn wrong_build_commit_arch_and_symlink_are_refused() {
    let source = scratch();
    let dist = scratch();
    let mut artifact = fixture(source.path(), LinuxTarget::Aarch64);
    assert!(artifact.validate("0.100.0", 101, &"a".repeat(40)).is_err());
    assert!(artifact.validate("0.100.0", 100, &"b".repeat(40)).is_err());
    artifact.target = LinuxTarget::X86_64.triple().into();
    assert!(
        artifact
            .verify_file(&source.path().join(&artifact.asset))
            .is_err()
    );
    let alias = dist.path().join("alias");
    std::os::unix::fs::symlink(source.path().join(&artifact.asset), &alias).unwrap();
    assert!(artifact.verify_file(&alias).is_err());
    assert!(check_identity("0.100.0", 0, &"a".repeat(40)).is_err());
    assert!(check_identity("0.100.0", 1, "short").is_err());
}

#[test]
fn the_channel_release_requires_every_signed_raw_asset_exactly_once() {
    let source = scratch();
    let artifact = fixture(source.path(), LinuxTarget::Aarch64);
    let mut published = crate::channel::required_asset_names("0.100.0", false, false);
    assert!(
        crate::channel::validate_channel_asset_set(
            &published,
            "0.100.0",
            false,
            false,
            std::slice::from_ref(&artifact.asset),
        )
        .is_err(),
        "a declared native executable the release does not carry is missing"
    );
    published.push(artifact.asset.clone());
    crate::channel::validate_channel_asset_set(
        &published,
        "0.100.0",
        false,
        false,
        std::slice::from_ref(&artifact.asset),
    )
    .unwrap();
    published.push(artifact.asset.clone());
    assert!(
        crate::channel::validate_channel_asset_set(
            &published,
            "0.100.0",
            false,
            false,
            &[artifact.asset],
        )
        .is_err(),
        "a duplicated native executable is refused"
    );
}

#[test]
fn a_real_cut_declares_its_linux_shape() {
    let parse =
        |args: &[&str]| crate::cli::parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    assert_eq!(parse(&["cut"]).unwrap_err(), crate::cli::MAC_ONLY_REFUSAL);
    assert!(parse(&["cut", "--mac-only"]).is_ok());
    assert!(parse(&["cut", "--linux-artifacts", "/private/output"]).is_ok());
    assert!(parse(&["cut", "--dry-run"]).is_ok());
    assert!(parse(&["cut", "--resume"]).is_ok());
    for refused in [
        &["cut", "--mac-only", "--linux-artifacts", "/private/output"][..],
        &["cut", "--resume", "--mac-only"],
        &["cut", "--resume", "--linux-artifacts", "/changed/output"],
        &["cut", "--linux-target", "aarch64"],
        &["cut", "--mac-only", "--linux-worker", "aarch64=h:/r"],
    ] {
        assert!(parse(refused).is_err(), "{refused:?}");
    }
}

/// NEGATIVE CONTROL: `Worker::parse` must refuse malformed `user@host` destinations,
/// root/empty/dot/traversal repository segments, and shell metacharacters.
#[test]
fn a_worker_spec_is_one_shell_safe_host_and_path() {
    let worker = Worker::parse("aarch64=builder@buildhost:/home/builder/src/aterm").unwrap();
    assert_eq!(worker.target, LinuxTarget::Aarch64.triple());
    assert_eq!(worker.destination, "builder@buildhost");
    assert_eq!(worker.repository, "/home/builder/src/aterm");
    assert_eq!(
        Worker::parse("x86_64=box:~/src/aterm").unwrap().target,
        LinuxTarget::X86_64.triple()
    );
    for bad in [
        "aarch64",
        "aarch64=host",
        "riscv64=host:/r",
        "aarch64=:/r",
        "aarch64=@host:/r",
        "aarch64=user@:/r",
        "aarch64=a@b@c:/r",
        "aarch64=user@-bad:/r",
        "aarch64=.:/r",
        "aarch64=-oProxyCommand=evil:/r",
        "aarch64=host;rm:/r",
        "aarch64=host:relative/path",
        "aarch64=host:/",
        "aarch64=host:~/",
        "aarch64=host:/r//sub",
        "aarch64=host:/r/",
        "aarch64=host:/r/.",
        "aarch64=host:~/.",
        "aarch64=host:/r/../etc",
        "aarch64=host:/r with space",
        "aarch64=host:/r$(id)",
        "aarch64=host:~root/r",
    ] {
        assert!(Worker::parse(bad).is_err(), "{bad:?} must be refused");
    }
}

#[test]
fn workers_must_build_a_declared_target_once() {
    let dir = scratch();
    let aarch64 = LinuxTarget::Aarch64.triple().to_string();
    let handoff = Handoff::new(
        dir.path(),
        std::slice::from_ref(&aarch64),
        &["aarch64=h:/r".into()],
    )
    .unwrap();
    assert_eq!(handoff.workers.len(), 1);
    assert!(
        Handoff::new(
            dir.path(),
            std::slice::from_ref(&aarch64),
            &["x86_64=h:/r".into()]
        )
        .is_err(),
        "a worker for an architecture the cut does not declare"
    );
    assert!(
        Handoff::new(
            dir.path(),
            &[],
            &["aarch64=a:/r".into(), "aarch64=b:/r".into()]
        )
        .is_err(),
        "two workers for one architecture"
    );
}

#[test]
fn only_this_cuts_valid_pair_counts_as_already_built() {
    let dir = scratch();
    let target = LinuxTarget::Aarch64;
    let commit = "a".repeat(40);
    assert!(
        !pair_is_current(dir.path(), target, "0.100.0", 100, &commit),
        "nothing there"
    );
    fixture(dir.path(), target);
    assert!(pair_is_current(dir.path(), target, "0.100.0", 100, &commit));
    assert!(!pair_is_current(
        dir.path(),
        target,
        "0.100.0",
        101,
        &commit
    ));
    assert!(!pair_is_current(
        dir.path(),
        target,
        "0.100.0",
        100,
        &"b".repeat(40)
    ));
    assert!(!pair_is_current(
        dir.path(),
        LinuxTarget::X86_64,
        "0.100.0",
        100,
        &commit
    ));
    let asset = dir.path().join(target.asset_name("0.100.0"));
    let mut bytes = fs::read(&asset).unwrap();
    bytes.truncate(70);
    fs::write(&asset, bytes).unwrap();
    assert!(!pair_is_current(
        dir.path(),
        target,
        "0.100.0",
        100,
        &commit
    ));
}

#[test]
fn the_worker_script_is_parsed_whole_before_it_runs() {
    assert!(WORKER_SCRIPT.starts_with("main() {\n"));
    assert!(WORKER_SCRIPT.trim_end().ends_with("main \"$@\" </dev/null"));
    assert!(WORKER_SCRIPT.contains("umask 077"));
    assert!(WORKER_SCRIPT.contains("mktemp -d"));
    if !std::path::Path::new("/bin/bash").exists() {
        aterm_spec::host::could_not_run("/bin/bash is not installed");
    }
    let status = command("/bin/bash")
        .args(["-n", "-c", WORKER_SCRIPT])
        .status()
        .unwrap();
    assert!(status.success(), "bash -n rejects the worker script");
}
