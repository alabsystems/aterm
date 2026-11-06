// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use std::cell::Cell;
use std::os::unix::fs::PermissionsExt as _;

use super::*;
use crate::select::Candidate;
use crate::sig::testkit;

const TRIPLE: &str = "aarch64-apple-darwin";

struct Fixture {
    layout: crate::testdir::ScratchLayout,
    want: Prerequisite,
    root: PathBuf,
    index: String,
    package: String,
    signature: Vec<u8>,
    index_reads: Cell<usize>,
    manifest_reads: Cell<usize>,
    after_first: Option<(Vec<u8>, Vec<u8>)>,
    archive: Option<Vec<u8>>,
    downloads: Cell<usize>,
}

impl Fixture {
    fn new(want: Prerequisite) -> Self {
        let layout = crate::testdir::layout("signed-prerequisite");
        let root = layout.build_dir(want.recipe().name, 7);
        std::fs::create_dir_all(root.join("bin")).unwrap();
        let executable = root.join("bin").join(want.executable());
        std::fs::write(&executable, b"synthetic fixture, never executed").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        crate::activate::install_tools(
            &layout,
            &root,
            &[crate::store::ToolName::new(want.executable()).unwrap()],
            crate::activate::Aliases::Off,
        )
        .unwrap();
        let tree = crate::tree::tree_root(&root).unwrap();
        let attr = format!(
            "machine_id = \"{}\"\nroster_seq = {}\n",
            testkit::MACHINE_ID,
            testkit::SEQ
        );
        let program = want.recipe().name;
        let index = format!(
            "schema=2\nindex_build=41\nvalid_until=\"2099-01-01T00:00:00Z\"\n{attr}[programs.{program}]\nrepo=\"{program}\"\n[[channels]]\nname=\"stable\"\npin={{ {program}=7 }}\n"
        );
        let package = format!(
            "schema=2\n{attr}program=\"{program}\"\nversion=\"{}\"\nbuild_number=7\nexposes=[\"{}\"]\n[[artifact]]\ntarget=\"{TRIPLE}\"\nkind=\"sysroot-bundle\"\nasset=\"fixture.tar.zst\"\nsha256=\"{}\"\ntree_root=\"{tree}\"\nsize=1\n",
            want.recipe().version,
            want.executable(),
            "a".repeat(64)
        );
        let signature = testkit::sign(&testkit::MACHINE_SEED, package.as_bytes());
        Self {
            layout,
            want,
            root,
            index,
            package,
            signature,
            index_reads: Cell::new(0),
            manifest_reads: Cell::new(0),
            after_first: None,
            archive: None,
            downloads: Cell::new(0),
        }
    }

    fn session(&self, triple: &'static str) -> Result<Session<'_>, crate::FlowError> {
        Session::resolve(
            self,
            &self.layout,
            &testkit::anchor(),
            BuildFloor {
                index_build: 0,
                roster_seq: 0,
            },
            testkit::NOW,
            triple,
        )
    }

    fn change_package(&mut self, from: &str, to: &str) {
        assert!(self.package.contains(from), "nonvacuous mutation");
        self.package = self.package.replace(from, to);
        self.signature = testkit::sign(&testkit::MACHINE_SEED, self.package.as_bytes());
    }

    fn executable(&self) -> PathBuf {
        self.root.join("bin").join(self.want.executable())
    }

    fn fresh_install(&mut self) {
        let old_root = crate::tree::tree_root(&self.root).unwrap();
        let body = b"#!/bin/true\n";
        std::fs::write(self.executable(), body).unwrap();
        let new_root = crate::tree::tree_root(&self.root).unwrap();
        let tar = crate::extract::fixtures::tar_bytes(&[(
            &format!("bin/{}", self.want.executable()),
            b'0',
            "",
            body,
            0o755,
        )]);
        let archive = crate::extract::fixtures::zstd_bytes(&tar);
        self.change_package(&old_root, &new_root);
        self.change_package(&"a".repeat(64), &crate::tree::sha256_bytes(&archive));
        self.change_package("size=1\n", &format!("size={}\n", archive.len()));
        self.archive = Some(archive);
        std::fs::remove_file(
            self.layout
                .shim(&crate::store::ToolName::new(self.want.executable()).unwrap()),
        )
        .unwrap();
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

impl Fetcher for Fixture {
    fn index_candidates(&self) -> Result<Vec<Candidate>, String> {
        self.index_reads.set(self.index_reads.get() + 1);
        let (roster_bytes, roster_sig) = testkit::published_roster();
        Ok(vec![Candidate {
            label: "fixture41".into(),
            index_bytes: self.index.as_bytes().to_vec(),
            sig: testkit::sign(&testkit::MACHINE_SEED, self.index.as_bytes()),
            roster_bytes,
            roster_sig,
        }])
    }

    fn pkg_manifest(
        &self,
        repo: &str,
        program: &str,
        build: u64,
    ) -> Result<(Vec<u8>, Vec<u8>), String> {
        assert_eq!(
            (repo, program, build),
            (self.want.recipe().name, self.want.recipe().name, 7)
        );
        self.manifest_reads.set(self.manifest_reads.get() + 1);
        if self.manifest_reads.get() > 1
            && let Some(pair) = &self.after_first
        {
            return Ok(pair.clone());
        }
        Ok((self.package.as_bytes().to_vec(), self.signature.clone()))
    }

    fn download(&self, _: &str, asset: &str, to: &Path) -> Result<(), String> {
        assert_eq!(asset, "fixture.tar.zst");
        self.downloads.set(self.downloads.get() + 1);
        let archive = self
            .archive
            .as_deref()
            .expect("a prerequisite lookup never downloads a payload");
        std::fs::write(to, archive).map_err(|e| e.to_string())
    }
}

#[test]
fn all_four_shared_recipes_admit_only_signed_bytes_and_hold_their_build_until_drop() {
    if !crate::testdir::isolated_lifecycle(
        "prerequisite::tests::all_four_shared_recipes_admit_only_signed_bytes_and_hold_their_build_until_drop",
    ) {
        return;
    }
    for want in [
        Prerequisite::Gitleaks,
        Prerequisite::Python,
        Prerequisite::OpenSslSigning,
        Prerequisite::Lake,
    ] {
        let fixture = Fixture::new(want);
        let session = fixture.session(TRIPLE).unwrap();
        let tool = session.acquire(want).unwrap();
        assert_eq!(tool.path(), fixture.executable());
        assert_eq!((tool.program(), tool.build()), (want.recipe().name, 7));
        assert!(tool.environment().is_empty());
        assert_eq!(fixture.index_reads.get(), 1);
        let subject = Subject::build(want.recipe().name, 7).unwrap();
        assert!(matches!(
            crate::lease::reclaim(&fixture.layout.prefix, &subject),
            crate::lease::Reclaim::Keep(_)
        ));
        drop(tool);
        let reclaimed = crate::lease::reclaim(&fixture.layout.prefix, &subject);
        assert!(
            matches!(reclaimed, crate::lease::Reclaim::Clear(_)),
            "{} after releasing its only owned guard: {reclaimed:?}",
            want.executable()
        );
    }
    #[cfg(any(target_vendor = "apple", target_os = "linux"))]
    inherited_description_keeps_a_dropped_prerequisite_until_actual_child_reap();
}

#[cfg(any(target_vendor = "apple", target_os = "linux"))]
fn inherited_description_keeps_a_dropped_prerequisite_until_actual_child_reap() {
    let fixture = Fixture::new(Prerequisite::Gitleaks);
    let tool = fixture
        .session(TRIPLE)
        .unwrap()
        .acquire(fixture.want)
        .unwrap();
    let subject = Subject::build("gitleaks", 7).unwrap();
    let child = aterm_uds::fork_window::Parked::park();
    drop(tool);
    let retained = crate::lease::reclaim(&fixture.layout.prefix, &subject);
    assert!(
        matches!(
            retained,
            crate::lease::Reclaim::Keep(crate::lease::Holders::Held(_))
        ),
        "fork inherited real custody despite CLOEXEC: {retained:?}"
    );
    drop(retained);
    assert!(
        child.release().success(),
        "the owned child execs and is reaped"
    );
    let reclaimed = crate::lease::reclaim(&fixture.layout.prefix, &subject);
    assert!(
        matches!(reclaimed, crate::lease::Reclaim::Clear(_)),
        "after final actual child close and reap: {reclaimed:?}"
    );
}

#[test]
fn provision_reuses_the_signed_pin_and_rechecks_current_bytes_with_a_lease() {
    let fixture = Fixture::new(Prerequisite::Gitleaks);
    crate::store::mark_build_ready(&fixture.root).unwrap();
    let session = fixture.session(TRIPLE).unwrap();
    assert!(session.install(fixture.want).unwrap());
    let held = session.acquire_all(&[fixture.want]).unwrap();
    assert_eq!(
        held.tool(fixture.want).unwrap().path(),
        fixture.executable()
    );
    assert_eq!(
        fixture.index_reads.get(),
        1,
        "one authenticated channel snapshot"
    );
    let subject = Subject::build("gitleaks", 7).unwrap();
    assert!(matches!(
        crate::lease::reclaim(&fixture.layout.prefix, &subject),
        crate::lease::Reclaim::Keep(_)
    ));
    drop(held);
    std::fs::write(fixture.executable(), b"a corrupted current installation").unwrap();
    // Being current is installer state, not permission to execute its bytes.
    assert!(session.install(fixture.want).unwrap());
    assert!(session.acquire_all(&[fixture.want]).is_err());
}

#[test]
fn provision_refuses_wrong_signed_recipe_before_changing_a_current_selection() {
    let mut fixture = Fixture::new(Prerequisite::Python);
    let before = std::fs::read(fixture.executable()).unwrap();
    let original = crate::lookup::read(Some(&fixture.layout), fixture.want.executable());
    fixture.change_package(
        &format!("version=\"{}\"", fixture.want.recipe().version),
        "version=\"0.0.0\"",
    );
    assert!(
        fixture
            .session(TRIPLE)
            .unwrap()
            .install(fixture.want)
            .unwrap_err()
            .contains("shared recipe")
    );
    assert_eq!(std::fs::read(fixture.executable()).unwrap(), before);
    assert!(same_selection(
        &original,
        &crate::lookup::read(Some(&fixture.layout), fixture.want.executable())
    ));
}

#[test]
fn provision_freezes_one_signed_document_through_fresh_install_and_admission() {
    for wrong_first in [false, true] {
        let mut fixture = Fixture::new(Prerequisite::Gitleaks);
        fixture.fresh_install();
        let valid = fixture.package.clone();
        let wrong = valid.replace(
            &format!("version=\"{}\"", fixture.want.recipe().version),
            "version=\"0.0.0\"",
        );
        assert_ne!(
            valid, wrong,
            "both documents carry the same program/build but different versions"
        );
        let (first, later) = if wrong_first {
            (wrong, valid)
        } else {
            (valid, wrong)
        };
        fixture.signature = testkit::sign(&testkit::MACHINE_SEED, first.as_bytes());
        fixture.package = first;
        fixture.after_first = Some((
            later.as_bytes().to_vec(),
            testkit::sign(&testkit::MACHINE_SEED, later.as_bytes()),
        ));
        let session = fixture.session(TRIPLE).unwrap();
        if wrong_first {
            assert!(
                session
                    .install(fixture.want)
                    .unwrap_err()
                    .contains("shared recipe")
            );
            assert!(!fixture.root.exists(), "refusal precedes installation");
            assert_eq!(fixture.downloads.get(), 0);
        } else {
            assert!(
                !session.install(fixture.want).unwrap(),
                "a real first installation"
            );
            let held = session.acquire_all(&[fixture.want]).unwrap();
            held.tool(fixture.want).unwrap().validate().unwrap();
            assert_eq!(
                crate::store::recorded_version(&fixture.root).as_deref(),
                Some(fixture.want.recipe().version)
            );
            assert_eq!(fixture.downloads.get(), 1);
        }
        assert_eq!(
            fixture.manifest_reads.get(),
            1,
            "the alternating signed document is never substituted"
        );
    }
}

#[test]
fn a_current_install_does_not_skip_snapshot_signature_parser_or_identity_checks() {
    let fixture = Fixture::new(Prerequisite::Gitleaks);
    crate::store::mark_build_ready(&fixture.root).unwrap();
    let session = fixture.session(TRIPLE).unwrap();
    let before = crate::lookup::read(Some(&fixture.layout), fixture.want.executable());
    let root = crate::tree::tree_root(&fixture.root).unwrap();
    for case in ["signature", "parser", "program", "build"] {
        let document = match case {
            "signature" => fixture.package.clone(),
            "parser" => "not a TOML document!".into(),
            "program" => fixture
                .package
                .replace("program=\"gitleaks\"", "program=\"foreign\""),
            "build" => fixture.package.replace("build_number=7", "build_number=8"),
            _ => unreachable!(),
        };
        let mut signature = testkit::sign(&testkit::MACHINE_SEED, document.as_bytes());
        if case == "signature" {
            signature[0] ^= 1;
        }
        let result = crate::flow::install_collecting_assets_with_snapshot(
            &fixture,
            &fixture.layout,
            &testkit::anchor(),
            &crate::InstallRequest {
                channel: "stable",
                program: "gitleaks",
                triple: TRIPLE,
                installed: Some(7),
            },
            BuildFloor::default(),
            testkit::NOW,
            Some(crate::flow::InstallSnapshot {
                index: session.index.clone(),
                package: Some((document.into_bytes(), signature)),
            }),
            &mut std::collections::BTreeMap::new(),
        );
        assert!(
            match case {
                "signature" => matches!(result, Err(crate::FlowError::PkgVerify)),
                "parser" => matches!(result, Err(crate::FlowError::PkgParse)),
                _ => matches!(result, Err(crate::FlowError::Mismatch)),
            },
            "{case}: {result:?}"
        );
        assert!(same_selection(
            &before,
            &crate::lookup::read(Some(&fixture.layout), fixture.want.executable())
        ));
        assert_eq!(crate::tree::tree_root(&fixture.root).unwrap(), root);
    }
    assert_eq!(
        fixture.manifest_reads.get(),
        0,
        "a provided snapshot never falls back to transport"
    );
    assert_eq!(fixture.downloads.get(), 0);
}

#[test]
fn authenticated_manifest_identity_and_local_bytes_are_both_required() {
    if !crate::testdir::isolated_lifecycle(
        "prerequisite::tests::authenticated_manifest_identity_and_local_bytes_are_both_required",
    ) {
        return;
    }
    for case in [
        "signature",
        "version",
        "program",
        "exposes",
        "target",
        "tree",
        "bytes",
        "alias",
        "environment",
    ] {
        let mut fixture = Fixture::new(Prerequisite::Python);
        match case {
            "signature" => fixture.signature[0] ^= 1,
            "version" => fixture.change_package(
                &format!("version=\"{}\"", Prerequisite::Python.recipe().version),
                "version=\"0.0.0\"",
            ),
            "program" => fixture.change_package("program=\"cpython\"", "program=\"foreign\""),
            "exposes" => fixture.change_package("exposes=[\"python3.14\"]", "exposes=[\"lake\"]"),
            "target" => fixture.change_package(TRIPLE, "x86_64-apple-darwin"),
            "tree" => {
                let tree = crate::tree::tree_root(&fixture.root).unwrap();
                fixture.change_package(&tree, &"b".repeat(64));
            }
            "bytes" => {
                std::fs::write(fixture.executable(), b"changed after signed installation").unwrap()
            }
            "alias" => {
                std::fs::write(fixture.root.join("bin/other"), b"another packaged binary").unwrap();
                std::fs::set_permissions(
                    fixture.root.join("bin/other"),
                    std::fs::Permissions::from_mode(0o755),
                )
                .unwrap();
                let tree = crate::tree::tree_root(&fixture.root).unwrap();
                let old = fixture
                    .package
                    .split("tree_root=\"")
                    .nth(1)
                    .unwrap()
                    .split('"')
                    .next()
                    .unwrap()
                    .to_string();
                fixture.change_package(&old, &tree);
                let shim = fixture
                    .layout
                    .shim(&crate::store::ToolName::new("python3.14").unwrap());
                std::fs::write(
                    shim,
                    crate::platform::sh_shim_content(&fixture.root.join("bin/other")),
                )
                .unwrap();
            }
            "environment" => fixture.change_package(
                "exposes=[",
                "shim_env=[\"DISABLE_AUTOUPDATER=1\"]\nexposes=[",
            ),
            _ => unreachable!(),
        }
        assert!(
            fixture
                .session(TRIPLE)
                .unwrap()
                .acquire(Prerequisite::Python)
                .is_err(),
            "{case}"
        );
        let subject = Subject::build("cpython", 7).unwrap();
        assert!(
            matches!(
                crate::lease::reclaim(&fixture.layout.prefix, &subject),
                crate::lease::Reclaim::Clear(_)
            ),
            "failed {case} leaked a lease"
        );
    }
}

#[test]
fn unsupported_hosts_stale_indexes_and_withdrawn_pins_are_explicit_refusals() {
    for want in [
        Prerequisite::Gitleaks,
        Prerequisite::Python,
        Prerequisite::OpenSslSigning,
        Prerequisite::Lake,
    ] {
        assert!(want.supports("aarch64-apple-darwin"));
        assert!(want.supports("x86_64-apple-darwin"));
        for target in ["aarch64-unknown-linux-gnu", "x86_64-unknown-linux-gnu"] {
            assert_eq!(want.supports(target), want == Prerequisite::Gitleaks);
        }
        for target in [
            "aarch64-pc-windows-msvc",
            "x86_64-pc-windows-msvc",
            "x86_64-unknown-linux-musl",
            "unknown",
        ] {
            assert!(!want.supports(target), "{} on {target}", want.executable());
        }
    }
    let fixture = Fixture::new(Prerequisite::Python);
    assert!(
        fixture
            .session("aarch64-unknown-linux-gnu")
            .unwrap()
            .acquire(Prerequisite::Python)
            .unwrap_err()
            .contains("unavailable")
    );
    let mut stale = Fixture::new(Prerequisite::Gitleaks);
    stale.index = stale
        .index
        .replace("2099-01-01T00:00:00Z", "2000-01-01T00:00:00Z");
    assert!(matches!(
        stale.session(TRIPLE),
        Err(crate::FlowError::Stale)
    ));
    let mut yanked = Fixture::new(Prerequisite::Gitleaks);
    yanked.index.push_str("yanked=[\"gitleaks@7\"]\n");
    assert!(
        yanked
            .session(TRIPLE)
            .unwrap()
            .acquire(Prerequisite::Gitleaks)
            .unwrap_err()
            .contains("withdrawn")
    );
}

#[test]
fn a_changed_shim_is_refused_after_lease_acquisition_and_drops_the_hold() {
    if !crate::testdir::isolated_lifecycle(
        "prerequisite::tests::a_changed_shim_is_refused_after_lease_acquisition_and_drops_the_hold",
    ) {
        return;
    }
    let fixture = Fixture::new(Prerequisite::Python);
    let selected = crate::lookup::read(Some(&fixture.layout), "python3.14");
    admit_selection(&selected, Prerequisite::Python, 7).unwrap();
    let shim = fixture
        .layout
        .shim(&crate::store::ToolName::new("python3.14").unwrap());
    std::fs::write(
        &shim,
        crate::platform::sh_shim_content(Path::new("/unmanaged/python3.14")),
    )
    .unwrap();
    assert!(
        hold_selection(
            &fixture.layout,
            Prerequisite::Python,
            7,
            &selected,
            "fixture lease race"
        )
        .is_err()
    );
    assert!(matches!(
        crate::lease::reclaim(
            &fixture.layout.prefix,
            &Subject::build("cpython", 7).unwrap()
        ),
        crate::lease::Reclaim::Clear(_)
    ));
}

#[test]
fn changed_images_and_lost_lease_custody_refuse_before_a_command_is_built() {
    for replace_slot in [false, true] {
        let fixture = Fixture::new(Prerequisite::Python);
        let session = fixture.session(TRIPLE).unwrap();
        let tool = session.acquire(Prerequisite::Python).unwrap();
        tool.validate().unwrap();
        let slot = tool.lease.path().to_path_buf();
        std::fs::remove_file(&slot).unwrap();
        if replace_slot {
            std::fs::write(&slot, b"replacement slot, not held\n").unwrap();
        }
        assert!(tool.command().err().unwrap().contains("lease lost custody"));
        assert!(
            fixture.executable().exists(),
            "the signed image remains present"
        );
    }
    for replace in [false, true] {
        let fixture = Fixture::new(Prerequisite::OpenSslSigning);
        let session = fixture.session(TRIPLE).unwrap();
        let tool = session.acquire(Prerequisite::OpenSslSigning).unwrap();
        tool.validate().unwrap();
        if replace {
            let replacement = fixture.root.join("replacement");
            std::fs::copy(fixture.executable(), &replacement).unwrap();
            std::fs::rename(replacement, fixture.executable()).unwrap();
        } else {
            std::fs::write(
                fixture.executable(),
                b"edited after admission, before execution",
            )
            .unwrap();
        }
        assert!(tool.command().is_err());
    }
}

#[test]
fn private_namespace_is_fixed_to_held_commands_without_exposing_lean_or_global_shim_flips() {
    let fixture = Fixture::new(Prerequisite::Lake);
    let session = fixture.session(TRIPLE).unwrap();
    let set = session
        .acquire_all(&[Prerequisite::Lake, Prerequisite::Lake])
        .unwrap();
    let entries: Vec<_> = std::fs::read_dir(set.directory.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(entries, [std::ffi::OsString::from("lake")]);
    assert_eq!(
        crate::platform::resolve_shim(&set.directory.path().join("lake")),
        Some(fixture.executable())
    );
    let global = fixture
        .layout
        .shim(&crate::store::ToolName::new("lake").unwrap());
    std::fs::write(
        &global,
        crate::platform::sh_shim_content(Path::new("/unmanaged/lake")),
    )
    .unwrap();
    let mut command = aterm_uds::spawnfd::command("/bin/sh");
    set.apply(&mut command, std::ffi::OsStr::new("/usr/bin:/bin"))
        .unwrap();
    assert_eq!(
        crate::platform::resolve_shim(&set.directory.path().join("lake")),
        Some(fixture.executable())
    );
    assert!(set.tool(Prerequisite::Lake).is_some());
    assert_eq!(fixture.index_reads.get(), 1);
    let python = Fixture::new(Prerequisite::Python);
    let held = python
        .session(TRIPLE)
        .unwrap()
        .acquire_all(&[Prerequisite::Python])
        .unwrap();
    for name in ["python3", "python3.14"] {
        assert_eq!(
            crate::platform::resolve_shim(&held.directory.path().join(name)),
            Some(python.executable())
        );
    }
    assert!(!held.directory.path().join("lean").exists());
    let initial = held
        .child_path(std::ffi::OsStr::new("/usr/bin:/bin"))
        .unwrap();
    assert_eq!(
        held.child_path(&initial).unwrap(),
        initial,
        "reapplying an operation namespace is idempotent"
    );
}
