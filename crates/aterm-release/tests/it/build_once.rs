// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Build once: a release builds its universal bundle once, under the number it reserved
//! (`artifact.rs`); `verify release` judges those bytes and records the pass under R's tree
//! with the executable's SHA-256; the cut signs a byte copy of them, never a rebuild. These
//! tests hold the release cache, the copy and the judges to that against real files and real
//! git. The release's step machine is tested in release_flow.rs, the ledger half in
//! ledger_race.rs.

use std::fs;
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::artifact::{self, Compilers, Complete, Identity};
use crate::ledger::GitCli;

const VERSION: &str = "0.102.0";

fn scratch() -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix("aterm-build-once-")
        .tempdir()
        .unwrap()
}

/// git with nothing of this machine's own configuration in it.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
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
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
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

/// A bundle shaped like the release's: the executable (mode 0755), its plist, a
/// resource, and an argv0 alias that is a symlink onto the executable.
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
        snapshot: "d".repeat(64),
        archs: "x86_64 arm64".to_string(),
        compiler_line: "Trust 1.95.0".to_string(),
        macho_sha256: artifact::macho_sha256(&app).unwrap(),
        bundle_digest: artifact::bundle_digest(&app).unwrap(),
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

/// The cut's copy is re-proved against the build's recorded digest, with every file, link and
/// mode, before anything signs it; a copy with any byte changed is refused, and a copy goes only
/// into a fresh place.
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

    // Negative controls: other bytes are never the verified ones.
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

/// Bytes that change in the store after the build are refused on every read — never
/// reused by a resumed release, never copied for a signature.
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

/// The record is written last: an entry without it is no build, and a record naming another
/// key is no record of this one.
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
}

/// The key is the version, reserved build number, release commit and both compilers: any one
/// changed is another build.
#[test]
fn the_key_is_what_the_bytes_were_built_as() {
    let base = artifact::key(&identity(7), &compilers());
    assert_eq!(base, artifact::key(&identity(7), &compilers()));
    assert_eq!(base.len(), 64);
    let mut others = vec![
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
    assert_eq!(others.len(), 5);
}

/// The build's records replace what `dist/` held under their names; one the build did not make
/// is removed there.
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

    // dsymutil failed for this build: no dSYM is faked, and the old one goes.
    fs::remove_dir_all(complete.dir(&store).join("aterm.dSYM")).unwrap();
    artifact::place_records(&complete.dir(&store), &dist, VERSION).unwrap();
    assert!(!dist.join("aterm.dSYM").exists());
}

/// The verified judge reads the record filed under the commit's tree and passes only one naming
/// exactly these bytes.
#[test]
fn the_verified_judge_reads_the_record_of_the_tree_on_these_bytes() {
    let dir = scratch();
    let repo = dir.path().join("work");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    fs::write(repo.join("README"), "aterm\n").unwrap();
    git(&repo, &["add", "README"]);
    git(&repo, &["commit", "-q", "-m", "R"]);
    let r = git(&repo, &["rev-parse", "HEAD"]);
    // a commit with the same tree: the record is about the tree
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "same tree"]);
    let twin = git(&repo, &["rev-parse", "HEAD"]);
    fs::write(repo.join("README"), "another tree\n").unwrap();
    git(&repo, &["commit", "-q", "-am", "another tree"]);
    let other = git(&repo, &["rev-parse", "HEAD"]);
    let g = GitCli::new(&repo);
    let macho = "ab".repeat(32);
    let judge = |commit: &str| crate::gates::verified(&g, commit, &macho).unwrap();

    let (yes, why) = judge(&r);
    assert!(!yes, "{why}");
    let at = crate::gates::record_verified(&g, &r, &"cd".repeat(32)).unwrap();
    let (yes, why) = judge(&r);
    assert!(!yes, "{why}");
    fs::write(&at, "not a record").unwrap();
    let (yes, why) = judge(&r);
    assert!(!yes, "{why}");

    crate::gates::record_verified(&g, &r, &macho).unwrap();
    let (yes, why) = judge(&r);
    assert!(yes, "{why}");
    assert!(judge(&twin).0, "the same tree is the same record");
    let (yes, why) = judge(&other);
    assert!(!yes, "another tree: {why}");
}

/// A clone with a published commit S, its release journal reserving build `n` of
/// [`VERSION`] as release commit C, and the bundle built under that reservation complete in
/// its release cache.
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
            // the release judged R's commit-only gates at its step 4
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

    fn verify(&self, macho: &str) {
        crate::gates::record_verified(&GitCli::new(&self.repo), &self.source, macho).unwrap();
    }

    /// The bundle a cut whose prover is `toolchain` would sign.
    fn bundle_for(&self, toolchain: &str) -> Result<crate::publish::VerifiedBundle, String> {
        crate::publish::verified_release_bundle(
            &GitCli::new(&self.repo),
            &self.repo,
            &self.source,
            VERSION,
            toolchain,
        )
        .map_err(|e| e.to_string())
    }

    /// The bundle a cut under the prover that compiled it would sign.
    fn bundle(&self) -> Result<crate::publish::VerifiedBundle, String> {
        self.bundle_for(&format!("/stage2/bin trustc {}", compilers().trustc))
    }
}

/// A cut signs only its release's verified bundle, decided before its claim: the published
/// commit's journal and reservation, the bundle complete under it, re-proved and compiled by the
/// cut's own prover, and the `verify release` pass for R's tree on that executable. Every
/// refusal says nothing was claimed.
#[test]
fn a_cut_signs_only_its_releases_verified_bundle() {
    let c = Clone::new();
    c.bundle().err().expect("nothing verified");

    // Verified: the bundle, its reservation and the judge's words.
    c.verify(&c.complete.macho_sha256);
    let bundle = c.bundle().unwrap();
    assert_eq!(bundle.reserved.build, 1_791_600_000);
    assert_eq!(bundle.reserved.commit, "e".repeat(40));
    assert_eq!(bundle.complete, c.complete);

    // Another release's journal (another published commit): nothing to sign.
    crate::publish::verified_release_bundle(
        &GitCli::new(&c.repo),
        &c.repo,
        &"0".repeat(40),
        VERSION,
        &format!("/stage2/bin trustc {}", compilers().trustc),
    )
    .err()
    .expect("another commit's release");

    // One compiler: a prover other than the trustc that compiled the bytes is refused.
    c.bundle_for(&format!("/other/bin trustc {}", "f".repeat(40)))
        .err()
        .expect("another prover");

    // What depends only on R's tree is read from the journal, never re-derived: a journal that
    // never judged R's commit-only gates fails closed.
    let path = Clone::journal_path(&c.repo);
    let mut j = crate::release::Journal::load(&path).unwrap().unwrap();
    let kept = j.clone();
    j.done.clear();
    j.save(&path).unwrap();
    c.bundle().err().expect("R's commit-only gates not judged");
    kept.save(&path).unwrap();
    assert!(c.bundle().is_ok(), "the facts back: the bundle again");

    // A journal whose bundle is not its reservation's (built under another number).
    let mut j = crate::release::Journal::load(&path).unwrap().unwrap();
    j.reserved.as_mut().unwrap().build += 1;
    j.save(&path).unwrap();
    c.bundle().err().expect("another reservation");
    kept.save(&path).unwrap();

    // The store's bytes changed after the build (and after `verify release`): refused.
    let store = artifact::store(&c.repo).unwrap();
    fs::write(
        c.complete.app(&store).join("Contents/Resources/Help.html"),
        "edited\n",
    )
    .unwrap();
    c.bundle().err().expect("changed bytes");
}

/// A journal that has not signed yet must name the measured bytes it will sign.
#[test]
fn an_unsigned_journal_names_the_bytes_it_will_sign() {
    // A journal that has not signed yet must name the measured bytes it will sign.
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
        manifest_signed: false,
        signature_required: false,
        signature_pubkey: None,
        verify_pubkey: None,
        signature_machine_id: None,
        release_id: None,
        release_intent: false,
        upload_intents: Vec::new(),
        done: vec!["lock".to_string()],
    };
    j.save(&path).unwrap_err();
    j.artifact = Some("aa".repeat(32));
    j.macho_sha256 = Some("bb".repeat(32));
    j.save(&path).unwrap();
    // Signed: nothing is left to sign, so nothing need be named (a recovery's journal).
    j.done.push("sign".to_string());
    j.artifact = None;
    j.macho_sha256 = None;
    j.save(&path).unwrap();
}

/// Between releases `ship build` builds HEAD from HEAD's objects: the workspace version and a
/// provisional number `max(ledger tail + 1, now)`, claimed nowhere.
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
    // A tail ahead of any clock: the number follows the ledger.
    fs::write(
        repo.join("RELEASES.ledger"),
        "# ledger\n9999999990 0.101.0\n",
    )
    .unwrap();
    git(&repo, &["add", "Cargo.toml", "RELEASES.ledger"]);
    git(&repo, &["commit", "-q", "-m", "head"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);
    // The worktree is not what is built: an edit there changes nothing.
    fs::write(
        repo.join("Cargo.toml"),
        "[workspace.package]\nversion = \"9.9.0\"\n",
    )
    .unwrap();
    let identity = artifact::provisional_identity(&GitCli::new(&repo)).unwrap();
    assert_eq!(
        identity,
        Identity {
            version: VERSION.to_string(),
            build: 9_999_999_991,
            commit: head,
        }
    );
}
