// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for single-build release artifact caching, bundle digest verification, and `verify release`'s per-check records.

use std::fs;
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};

use crate::artifact::{self, Compilers, Complete, Identity};
use crate::ledger::testing::Git;

const VERSION: &str = "0.102.0";

fn scratch() -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix("aterm-build-once-")
        .tempdir()
        .unwrap()
}

/// Runs hermetic `git` ([`crate::ledger::testing::git`]) in `dir`.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = crate::ledger::testing::git()
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// Writes a fake app bundle with an executable (`0755`), `aterm-ctl` symlink, `Info.plist`, and help resource.
fn fake_bundle(app: &Path, exe: &[u8]) {
    let macos = app.join("Contents/MacOS");
    fs::create_dir_all(&macos).unwrap();
    fs::create_dir_all(app.join("Contents/Resources")).unwrap();
    fs::write(macos.join("aterm"), exe).unwrap();
    fs::set_permissions(macos.join("aterm"), fs::Permissions::from_mode(0o755)).unwrap();
    symlink("aterm", macos.join("aterm-ctl")).unwrap();
    fs::write(app.join("Contents/Info.plist"), "<plist>build</plist>\n").unwrap();
    fs::write(app.join("Contents/Resources/Help.html"), "help\n").unwrap();
}

fn identity(build: u64) -> Identity {
    Identity {
        version: VERSION.to_string(),
        build,
        commit: "c".repeat(40),
    }
}

fn compilers() -> Compilers {
    Compilers {
        trustc: "a".repeat(40),
        rustc: "b".repeat(40),
        inputs: "c".repeat(64),
    }
}

/// A complete store entry for `identity`, as `artifact::build` leaves one, record last.
fn complete_entry(store: &Path, identity: &Identity, exe: &[u8]) -> Complete {
    let key = artifact::key(identity, &compilers());
    let dir = store.join(&key);
    fs::create_dir_all(&dir).unwrap();
    let app = artifact::app_in(&dir);
    fake_bundle(&app, exe);
    let dwarf = dir.join("aterm.dSYM/Contents/Resources/DWARF");
    fs::create_dir_all(&dwarf).unwrap();
    fs::write(dwarf.join("aterm"), b"dwarf").unwrap();
    fs::write(dir.join(format!("aterm-{VERSION}-dSYM.zip")), b"zip").unwrap();
    fs::write(
        dir.join(crate::buildplan::x86_64_probe_asset_name(VERSION)),
        b"probe",
    )
    .unwrap();
    let complete = Complete {
        schema: artifact::SCHEMA.to_string(),
        key,
        version: identity.version.clone(),
        build: identity.build,
        commit: identity.commit.clone(),
        trustc: compilers().trustc,
        rustc: compilers().rustc,
        inputs: compilers().inputs,
        snapshot: "d".repeat(64),
        archs: "x86_64 arm64".to_string(),
        compiler_line: "Trust 1.95.0".to_string(),
        macho_sha256: artifact::macho_sha256(&app).unwrap(),
        bundle_digest: artifact::bundle_digest(&app).unwrap(),
        content: None,
        slices: Vec::new(),
        index_hint: None,
    };
    fs::write(
        dir.join(artifact::COMPLETE_FILE),
        aterm_toml::to_string(&complete).unwrap(),
    )
    .unwrap();
    complete
}

/// `artifact::copy_bundle` and `artifact::prove` verify the bundle's executable digest, files, symlinks, and permissions.
#[test]
fn the_cut_signs_a_byte_copy_of_exactly_the_measured_bytes() {
    let dir = scratch();
    let store = dir.path().join("artifacts");
    let complete = complete_entry(&store, &identity(1_791_600_000), b"the measured executable");
    let read = artifact::read(&store, &complete.key)
        .unwrap()
        .expect("a COMPLETE entry");
    assert_eq!(read, complete);

    let staged = dir.path().join("dist/cut-1791600000.noindex/aterm.app");
    fs::create_dir_all(staged.parent().unwrap()).unwrap();
    artifact::copy_bundle(&read, &read.app(&store), &staged).unwrap();
    assert_eq!(
        artifact::macho_sha256(&staged).unwrap(),
        complete.macho_sha256,
        "the copy's executable is the measured one"
    );
    assert_eq!(
        artifact::bundle_digest(&staged).unwrap(),
        complete.bundle_digest
    );
    assert_eq!(
        fs::read_link(staged.join("Contents/MacOS/aterm-ctl")).unwrap(),
        PathBuf::from("aterm"),
        "an alias stays a link"
    );
    assert_eq!(
        fs::metadata(staged.join("Contents/MacOS/aterm"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o755
    );
    artifact::copy_bundle(&read, &read.app(&store), &staged).unwrap_err();

    fs::write(staged.join("Contents/Resources/Help.html"), "edited\n").unwrap();
    artifact::prove(&complete, &staged).unwrap_err();
    fs::write(staged.join("Contents/MacOS/aterm"), b"another executable").unwrap();
    let err = artifact::prove(&complete, &staged).unwrap_err();
    assert!(
        err.contains("not the") && err.contains(&complete.macho_sha256),
        "{err}"
    );
    fs::set_permissions(
        read.app(&store).join("Contents/MacOS/aterm"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    assert!(
        artifact::read(&store, &complete.key).is_err(),
        "a mode is part of the bytes"
    );
}

/// `artifact::read` rejects a store entry whose bundle bytes changed after completion.
#[test]
fn bytes_changed_in_the_store_after_the_build_are_never_reused_or_signed() {
    let dir = scratch();
    let store = dir.path().join("artifacts");
    let complete = complete_entry(&store, &identity(7), b"built");
    fs::write(
        complete.app(&store).join("Contents/MacOS/aterm"),
        b"patched afterwards",
    )
    .unwrap();
    artifact::read(&store, &complete.key).unwrap_err();
}

/// A store entry without its completion record returns `None`, and a mismatched key returns an error.
#[test]
fn an_entry_without_its_record_is_no_build() {
    let dir = scratch();
    let store = dir.path().join("artifacts");
    let complete = complete_entry(&store, &identity(7), b"built");
    let record = complete.dir(&store).join(artifact::COMPLETE_FILE);
    let text = fs::read_to_string(&record).unwrap();
    fs::remove_file(&record).unwrap();
    assert_eq!(artifact::read(&store, &complete.key).unwrap(), None);
    assert_eq!(artifact::read(&store, &"0".repeat(64)).unwrap(), None);
    fs::write(&record, text.replace(&complete.key, &"e".repeat(64))).unwrap();
    artifact::read(&store, &complete.key).unwrap_err();
    fs::write(&record, text.replace(&complete.inputs, &"e".repeat(64))).unwrap();
    assert!(
        artifact::read(&store, &complete.key)
            .unwrap_err()
            .contains("build inputs")
    );
}

/// `artifact::key` changes whenever the version, build number, commit, or compiler hashes change.
#[test]
fn the_key_is_what_the_bytes_were_built_as() {
    let base = artifact::key(&identity(7), &compilers());
    assert_eq!(base, artifact::key(&identity(7), &compilers()));
    assert_eq!(base.len(), 64);
    let mut others = vec![
        artifact::key(
            &identity(7),
            &Compilers {
                inputs: "d".repeat(64),
                ..compilers()
            },
        ),
        artifact::key(&identity(8), &compilers()),
        artifact::key(
            &Identity {
                commit: "f".repeat(40),
                ..identity(7)
            },
            &compilers(),
        ),
        artifact::key(
            &Identity {
                version: "0.103.0".to_string(),
                ..identity(7)
            },
            &compilers(),
        ),
        artifact::key(
            &identity(7),
            &Compilers {
                trustc: "0".repeat(40),
                ..compilers()
            },
        ),
        artifact::key(
            &identity(7),
            &Compilers {
                rustc: "0".repeat(40),
                ..compilers()
            },
        ),
    ];
    assert!(others.iter().all(|k| *k != base));
    others.sort();
    others.dedup();
    assert_eq!(others.len(), 6);
}

/// `artifact::place_records` replaces existing `dist/` records and removes absent ones.
#[test]
fn the_records_beside_the_bundle_replace_dists() {
    let dir = scratch();
    let store = dir.path().join("artifacts");
    let complete = complete_entry(&store, &identity(7), b"built");
    let dist = dir.path().join("dist");
    fs::create_dir_all(dist.join("aterm.dSYM")).unwrap();
    fs::write(dist.join("aterm.dSYM/stale"), b"old").unwrap();
    artifact::place_records(&complete.dir(&store), &dist, VERSION).unwrap();
    assert_eq!(
        fs::read(dist.join("aterm.dSYM/Contents/Resources/DWARF/aterm")).unwrap(),
        b"dwarf"
    );
    assert!(
        !dist.join("aterm.dSYM/stale").exists(),
        "replaced, not merged"
    );
    assert_eq!(
        fs::read(dist.join(format!("aterm-{VERSION}-dSYM.zip"))).unwrap(),
        b"zip"
    );
    let probe = crate::buildplan::x86_64_probe_asset_name(VERSION);
    assert_eq!(fs::read(dist.join(&probe)).unwrap(), b"probe");

    fs::remove_dir_all(complete.dir(&store).join("aterm.dSYM")).unwrap();
    artifact::place_records(&complete.dir(&store), &dist, VERSION).unwrap();
    assert!(!dist.join("aterm.dSYM").exists());
}

/// A record past every whole-file read bound (a release's DWARF runs to 1.7 GB) is copied
/// whole: `place_records` streams it.
#[test]
fn a_record_larger_than_any_read_bound_is_placed_whole() {
    let dir = scratch();
    let store = dir.path().join("artifacts");
    let complete = complete_entry(&store, &identity(7), b"built");
    let dwarf = complete
        .dir(&store)
        .join("aterm.dSYM/Contents/Resources/DWARF/aterm");
    let size = crate::durable::MAX_RELEASE_FILE_BYTES + 1;
    fs::File::options()
        .write(true)
        .open(&dwarf)
        .unwrap()
        .set_len(size)
        .unwrap();
    let dist = dir.path().join("dist");
    fs::create_dir_all(&dist).unwrap();
    artifact::place_records(&complete.dir(&store), &dist, VERSION).unwrap();
    assert_eq!(
        fs::metadata(dist.join("aterm.dSYM/Contents/Resources/DWARF/aterm"))
            .unwrap()
            .len(),
        size
    );
}

/// `gates::verified` answers from one record per check of the commit's tree under the seal, the
/// bundle's `measure` on these bytes: its words name what is missing; another seal, another
/// tree or a garbled record stands for nothing, and a legacy single record at
/// `verified/<tree>` is replaced by the next run's records.
#[test]
fn the_verified_judge_reads_each_checks_record_of_the_tree_on_these_bytes() {
    let dir = scratch();
    let repo = dir.path().join("work");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    fs::write(repo.join("README"), "aterm\n").unwrap();
    git(&repo, &["add", "README"]);
    git(&repo, &["commit", "-q", "-m", "R"]);
    let r = git(&repo, &["rev-parse", "HEAD"]);
    let tree = git(&repo, &["rev-parse", "HEAD^{tree}"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "same tree"]);
    let twin = git(&repo, &["rev-parse", "HEAD"]);
    fs::write(repo.join("README"), "another tree\n").unwrap();
    git(&repo, &["commit", "-q", "-am", "another tree"]);
    let other = git(&repo, &["rev-parse", "HEAD"]);
    let g = Git::new(&repo);
    let macho = "ab".repeat(32);
    let inputs = compilers().inputs;
    let judge = |commit: &str| {
        crate::gates::verified_with_inputs(&g, commit, &macho, || Ok(compilers().inputs)).unwrap()
    };
    let missing = |commit: &str| {
        crate::check_record::checks(&g, commit, Some(&macho), || Ok(compilers().inputs))
            .unwrap()
            .missing
    };
    let ok = |_: &str| Some("ok");

    assert!(!judge(&r).0);
    assert_eq!(missing(&r), crate::check_record::release_names());
    crate::check_record::record_run(&g, &r, &"cd".repeat(32), &inputs, 0, ok);
    let (yes, why) = judge(&r);
    assert!(!yes, "another executable's measure: {why}");
    assert_eq!(missing(&r), ["measure"]);

    crate::check_record::record_run(&g, &r, &macho, &inputs, 0, ok);
    let (yes, why) = judge(&r);
    assert!(yes, "{why}");
    assert!(
        !crate::gates::verified_with_inputs(&g, &r, &macho, || Ok("changed compiler bytes".into()))
            .unwrap()
            .0,
        "another seal"
    );
    assert!(judge(&twin).0, "the same tree is the same records");
    let (yes, why) = judge(&other);
    assert!(!yes, "another tree: {why}");

    let verified = crate::check_record::root(&g).unwrap();
    fs::write(
        crate::check_record::path(&verified, &tree, &inputs, "lint", None),
        "not a record",
    )
    .unwrap();
    assert!(!judge(&r).0);
    assert_eq!(missing(&r), ["lint"]);

    fs::remove_dir_all(verified.join(&tree)).unwrap();
    fs::write(
        verified.join(&tree),
        format!("commit = {r:?}\nexecutable = {macho:?}\ninputs = {inputs:?}\n"),
    )
    .unwrap();
    let (yes, why) = judge(&r);
    assert!(!yes, "a legacy single record stands for no check: {why}");
    crate::check_record::record_run(&g, &r, &macho, &inputs, 0, ok);
    assert!(
        verified.join(&tree).is_dir(),
        "replaced by the run's records"
    );
    assert!(judge(&r).0);
}

/// Scratch git clone with a published commit, release journal reservation, and completed artifact store entry.
struct Clone {
    _dir: aterm_tempfile::TempDir,
    repo: PathBuf,
    source: String,
    complete: Complete,
}

impl Clone {
    fn new() -> Self {
        let dir = scratch();
        let repo = dir.path().join("work");
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        fs::write(repo.join("README"), "aterm\n").unwrap();
        git(&repo, &["add", "README"]);
        git(&repo, &["commit", "-q", "-m", "the published commit"]);
        let source = git(&repo, &["rev-parse", "HEAD"]);
        let reserved = Identity {
            version: VERSION.to_string(),
            build: 1_791_600_000,
            commit: "e".repeat(40),
        };
        let store = artifact::store(&repo).unwrap();
        let complete = complete_entry(&store, &reserved, b"the measured executable");
        crate::release::Journal {
            version: VERSION.to_string(),
            commit: source.clone(),
            reserved: Some(crate::release::Reservation {
                build: reserved.build,
                commit: reserved.commit.clone(),
                line: format!("{} {VERSION}", reserved.build),
            }),
            artifact: Some(crate::release::Artifact {
                key: complete.key.clone(),
                app: complete.app(&store),
                macho_sha256: complete.macho_sha256.clone(),
                build: reserved.build,
            }),
            done: vec![crate::release::COMMIT_GATES.to_string()],
            ..crate::release::Journal::default()
        }
        .save(&Self::journal_path(&repo))
        .unwrap();
        Self {
            _dir: dir,
            repo,
            source,
            complete,
        }
    }

    fn journal_path(repo: &Path) -> PathBuf {
        repo.join(".git")
            .join(crate::release::RELEASE_DIR)
            .join(crate::release::JOURNAL_FILE)
    }

    /// Every check a release requires recorded ok for the published commit's tree, the bundle's
    /// on `macho`.
    fn verify(&self, macho: &str) {
        crate::check_record::record_run(
            &Git::new(&self.repo),
            &self.source,
            macho,
            &compilers().inputs,
            0,
            |_| Some("ok"),
        );
    }

    /// The bundle a cut whose prover is `toolchain` would sign.
    fn bundle_for(&self, toolchain: &str) -> Result<crate::publish::VerifiedBundle, String> {
        crate::publish::verified_release_bundle_with_inputs(
            &Git::new(&self.repo),
            &self.repo,
            &self.source,
            VERSION,
            toolchain,
            || Ok(compilers().inputs),
        )
        .map_err(|e| e.to_string())
    }

    /// The bundle a cut under the prover that compiled it would sign.
    fn bundle(&self) -> Result<crate::publish::VerifiedBundle, String> {
        self.bundle_for(&format!("/stage2/bin trustc {}", compilers().trustc))
    }
}

/// `publish::verified_release_bundle` requires a matching commit, compiler, commit-gate record, reservation, and unmodified store bundle.
#[test]
fn a_cut_signs_only_its_releases_verified_bundle() {
    let c = Clone::new();
    c.bundle().err().expect("nothing verified");

    c.verify(&c.complete.macho_sha256);
    let bundle = c.bundle().unwrap();
    assert_eq!(bundle.reserved.build, 1_791_600_000);
    assert_eq!(bundle.reserved.commit, "e".repeat(40));
    assert_eq!(bundle.complete, c.complete);

    // The fixture/policy fact is adopted only through the bundle reader's seal
    // proof, even when the journal still names the same source and reservation.
    let changed_seal = crate::publish::verified_release_bundle_with_inputs(
        &Git::new(&c.repo),
        &c.repo,
        &c.source,
        VERSION,
        &format!("/stage2/bin trustc {}", compilers().trustc),
        || Ok("another seal".into()),
    );
    assert!(
        changed_seal.is_err(),
        "another seal must not adopt commit gates"
    );

    crate::publish::verified_release_bundle(
        &Git::new(&c.repo),
        &c.repo,
        &"0".repeat(40),
        VERSION,
        &format!("/stage2/bin trustc {}", compilers().trustc),
    )
    .err()
    .expect("another commit's release");

    c.bundle_for(&format!("/other/bin trustc {}", "f".repeat(40)))
        .err()
        .expect("another prover");

    let path = Clone::journal_path(&c.repo);
    let mut j = crate::release::Journal::load(&path).unwrap().unwrap();
    let kept = j.clone();
    j.done.clear();
    j.save(&path).unwrap();
    c.bundle().err().expect("R's commit-only gates not judged");
    kept.save(&path).unwrap();
    assert!(c.bundle().is_ok(), "the facts back: the bundle again");

    let mut j = crate::release::Journal::load(&path).unwrap().unwrap();
    j.reserved.as_mut().unwrap().build += 1;
    j.save(&path).unwrap();
    c.bundle().err().expect("another reservation");
    kept.save(&path).unwrap();

    let store = artifact::store(&c.repo).unwrap();
    fs::write(
        c.complete.app(&store).join("Contents/Resources/Help.html"),
        "edited\n",
    )
    .unwrap();
    c.bundle().err().expect("changed bytes");
}

/// NEGATIVE CONTROL: a COMMIT_GATES fact cannot authorize a claim when a
/// required check of the source tree is red. Omitting verified_with_inputs in
/// the cut's bundle reader would accept this bundle.
#[test]
fn adopting_commit_gates_does_not_adopt_a_red_tree() {
    let c = Clone::new();
    crate::check_record::record_run(
        &Git::new(&c.repo),
        &c.source,
        &c.complete.macho_sha256,
        &compilers().inputs,
        1,
        |name| Some(if name == "lint" { "fail" } else { "ok" }),
    );
    let error = c.bundle().err().expect("red tree must refuse before claim");
    assert!(error.contains("UNVERIFIED"), "{error}");
}

/// Private preparation keeps every build proof while allowing missing verification records.
/// NEGATIVE CONTROLS: requiring READY here prevents the overlap; weakening the ordinary cut
/// gate makes `bundle` succeed before `verify`; omitting the build identity admits changed bytes.
#[test]
fn private_preparation_needs_built_bytes_but_the_claim_still_needs_verified_bytes() {
    let c = Clone::new();
    let prepared = || {
        crate::publish::release_bundle_with_inputs(
            &Git::new(&c.repo),
            &c.repo,
            &c.source,
            VERSION,
            &format!("/stage2/bin trustc {}", compilers().trustc),
            || Ok(compilers().inputs),
            false,
        )
    };
    assert!(c.bundle().is_err());
    assert_eq!(prepared().unwrap().complete, c.complete);
    c.verify(&c.complete.macho_sha256);
    assert!(c.bundle().is_ok());
    let store = artifact::store(&c.repo).unwrap();
    fs::write(
        c.complete.app(&store).join("Contents/MacOS/aterm"),
        b"other bytes",
    )
    .unwrap();
    assert!(prepared().is_err());
    assert!(c.bundle().is_err());
}

/// A journal that has not signed yet must name the measured bytes it will sign.
#[test]
fn an_unsigned_journal_names_the_bytes_it_will_sign() {
    let dir = scratch();
    let path = dir.path().join("cut-state.toml");
    let mut j = crate::publish::Journal {
        format: crate::publish::JOURNAL_FORMAT,
        version: VERSION.to_string(),
        build_number: 1_791_600_000,
        commit: "e".repeat(40),
        min_build: None,
        artifact: None,
        macho_sha256: None,
        linux: None,
        windows: None,
        manifest_signed: false,
        signature_required: false,
        signature_pubkey: None,
        verify_pubkey: None,
        signature_machine_id: None,
        release_id: None,
        release_intent: false,
        upload_intents: Vec::new(),
        delete_intent: None,
        done: vec!["lock".to_string()],
    };
    j.save(&path).unwrap_err();
    j.artifact = Some("aa".repeat(32));
    j.macho_sha256 = Some("bb".repeat(32));
    j.save(&path).unwrap();
    j.done.push("sign".to_string());
    j.artifact = None;
    j.macho_sha256 = None;
    j.save(&path).unwrap();
}

/// `artifact::provisional_identity` reads the committed `HEAD` version and assigns `max(ledger_tail + 1, now)`.
#[test]
fn between_releases_ship_build_builds_head_under_a_provisional_number() {
    let dir = scratch();
    let repo = dir.path().join("work");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    fs::write(
        repo.join("Cargo.toml"),
        "[workspace.package]\nversion = \"0.102.0\"\n",
    )
    .unwrap();
    fs::write(
        repo.join("RELEASES.ledger"),
        "# ledger\n9999999990 0.101.0\n",
    )
    .unwrap();
    git(&repo, &["add", "Cargo.toml", "RELEASES.ledger"]);
    git(&repo, &["commit", "-q", "-m", "head"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);
    fs::write(
        repo.join("Cargo.toml"),
        "[workspace.package]\nversion = \"9.9.0\"\n",
    )
    .unwrap();
    let identity = artifact::provisional_identity(&Git::new(&repo)).unwrap();
    assert_eq!(
        identity,
        Identity {
            version: VERSION.to_string(),
            build: 9_999_999_991,
            commit: head,
        }
    );
}
