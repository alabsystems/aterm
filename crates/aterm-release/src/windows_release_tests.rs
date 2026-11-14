// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_winship::handoff::{Lane, fixture};
use std::fs;

const VERSION: &str = "0.100.0";
const BUILD: u64 = 100;
const TRUST_COMMIT: &str = "29ebb403de7a102850d6cb10a62d5cc3baf723ed";

fn commit() -> String {
    "a".repeat(40)
}

fn sources() -> Sources {
    Sources {
        tree: "4b825dc642cb6eb9a060e54bf8d69288fbee4904".to_string(),
        stock: fixture::STOCK.to_string(),
        trust_commit: Some(TRUST_COMMIT.to_string()),
    }
}

fn cut<'a>(commit: &'a str, sources: &'a Sources) -> Cut<'a> {
    Cut {
        version: VERSION,
        build: BUILD,
        commit,
        source_tree: &sources.tree,
    }
}

fn scratch() -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix("aterm-windows-handoff-")
        .tempdir()
        .unwrap()
}

/// A handoff directory holding a valid pair for this cut, in `lane`; its receipt.
fn carried(dir: &Path, lane: Lane) -> Receipt {
    let commit = commit();
    let sources = sources();
    let zip = fixture::portable();
    let receipt = fixture::receipt(&zip, &cut(&commit, &sources), lane, TRUST_COMMIT);
    fixture::carry(dir, &zip, &receipt).unwrap();
    receipt
}

/// The handoff of `dir`, bound to the digest of the carried zip as the person named it.
fn bound(dir: &Path) -> Handoff {
    Handoff::new(dir, Some(&fixture::sha256(&fixture::portable()))).unwrap()
}

fn manifest() -> Manifest {
    Manifest::parse(&format!(
        "schema=1\nversion=\"{VERSION}\"\nbuild_number={BUILD}\ncommit=\"{}\"\n\
         dmg=\"aterm-{VERSION}.dmg\"\nsha256=\"{}\"\nteam_id=\"\"\n",
        commit(),
        "f".repeat(64)
    ))
    .unwrap()
}

#[test]
fn a_carry_of_the_cut_is_staged_frozen_and_signed() {
    for lane in [Lane::Stock, Lane::Trust] {
        let source = scratch();
        let dist = scratch();
        let receipt = carried(source.path(), lane);
        let mut handoff = bound(source.path());
        handoff
            .import(dist.path(), VERSION, BUILD, &commit(), &sources())
            .unwrap();
        assert_eq!(handoff.artifact.as_ref(), Some(&receipt));
        let verifier = verifier_asset(VERSION);
        assert_eq!(
            verifier,
            "aterm-bootstrap-verify-0.100.0-windows-x86_64.exe"
        );
        assert_eq!(
            handoff.asset_names(),
            vec![receipt.asset.clone(), verifier.clone()]
        );
        assert_eq!(
            fs::read(dist.path().join(&receipt.asset)).unwrap(),
            fixture::portable()
        );
        // The verifier the zip carries is staged beside it as its own asset.
        assert_eq!(
            fs::read(dist.path().join(&verifier)).unwrap(),
            fixture::verifier()
        );
        assert_eq!(
            handoff.verifier,
            Some(Verifier {
                sha256: fixture::sha256(&fixture::verifier()),
                size: fixture::verifier().len() as u64,
            })
        );
        handoff
            .verify_staged(dist.path(), VERSION, BUILD, &commit())
            .unwrap();
        // The journal keeps it whole.
        let frozen: Handoff =
            aterm_toml::from_str(&aterm_toml::to_string(&handoff).unwrap()).unwrap();
        assert_eq!(frozen, handoff);
        let mut signed = manifest();
        handoff.stamp(&mut signed).unwrap();
        assert_eq!(
            manifest_asset_names(&signed).unwrap(),
            vec![receipt.asset.clone(), verifier.clone()]
        );
        // The appcast signs the verifier get-aterm.ps1's pin is read back from.
        let (receipts, interface) = crate::windows_pin::signed_windows_verifiers(&signed)
            .unwrap()
            .expect("the appcast signs a verifier");
        assert_eq!(interface, pin::WINDOWS_INTERFACE);
        assert_eq!(receipts[0].sha256, fixture::sha256(&fixture::verifier()));
        verify_manifest_handoff(&signed, Some(&handoff)).unwrap();
        assert!(verify_manifest_handoff(&signed, None).is_err());
        verify_manifest_handoff(&manifest(), None).unwrap();
        assert!(verify_manifest_handoff(&manifest(), Some(&handoff)).is_err());
        let mut wrong_zip = signed.clone();
        wrong_zip.windows_x86_64_size = Some(receipt.size + 1);
        assert!(verify_manifest_handoff(&wrong_zip, Some(&handoff)).is_err());
        let mut wrong_verifier = signed.clone();
        wrong_verifier.windows_verifier_x86_64_sha256 = Some("e".repeat(64));
        assert!(verify_manifest_handoff(&wrong_verifier, Some(&handoff)).is_err());
        let mut no_verifier = signed.clone();
        no_verifier.windows_verifier_x86_64 = None;
        no_verifier.windows_verifier_x86_64_sha256 = None;
        no_verifier.windows_verifier_x86_64_size = None;
        no_verifier.windows_verifier_interface = None;
        assert!(verify_manifest_handoff(&no_verifier, Some(&handoff)).is_err());
    }
}

#[test]
fn the_verifier_is_the_one_the_frozen_zip_carries() {
    let source = scratch();
    let dist = scratch();
    carried(source.path(), Lane::Stock);
    let mut handoff = bound(source.path());
    handoff
        .import(dist.path(), VERSION, BUILD, &commit(), &sources())
        .unwrap();
    let staged = dist.path().join(verifier_asset(VERSION));
    // Staged bytes changed after the freeze.
    let mut bytes = fs::read(&staged).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    fs::write(&staged, &bytes).unwrap();
    assert!(
        handoff
            .verify_staged(dist.path(), VERSION, BUILD, &commit())
            .is_err(),
        "a changed staged verifier passed"
    );
    // A journal that froze another verifier digest, with matching staged bytes: still not
    // the one the zip carries.
    let mut forged = handoff.clone();
    forged.verifier = Some(Verifier {
        sha256: fixture::sha256(&bytes),
        size: bytes.len() as u64,
    });
    let err = forged
        .verify_staged(dist.path(), VERSION, BUILD, &commit())
        .unwrap_err();
    assert!(err.contains("carries"), "{err}");
    // A journal that froze the zip and lost the verifier signs nothing.
    let mut lost = handoff.clone();
    lost.verifier = None;
    assert!(lost.stamp(&mut manifest()).is_err());
    assert!(
        lost.validate_identity(VERSION, BUILD, &commit(), true)
            .unwrap_err()
            .contains("verifier")
    );
}

#[test]
fn a_zip_without_its_verifier_is_refused() {
    let commit = commit();
    let sources = sources();
    let bare = fixture::zip(&[
        ("aterm/aterm-gui.exe", fixture::image(2)),
        ("aterm/aterm.exe", fixture::image(3)),
    ]);
    let receipt = fixture::receipt(&bare, &cut(&commit, &sources), Lane::Stock, TRUST_COMMIT);
    let source = scratch();
    let dist = scratch();
    fixture::carry(source.path(), &bare, &receipt).unwrap();
    let mut handoff = Handoff::new(source.path(), Some(&receipt.sha256)).unwrap();
    let err = handoff
        .import(dist.path(), VERSION, BUILD, &commit, &sources)
        .unwrap_err();
    assert!(err.contains(handoff::VERIFIER_MEMBER), "{err}");
    assert!(handoff.artifact.is_none() && handoff.verifier.is_none());
    // Refused at admission, before the zip is staged, not later by the cut's own extraction.
    assert!(
        !dist.path().join(&receipt.asset).exists(),
        "the zip was staged"
    );
    assert!(!dist.path().join(verifier_asset(VERSION)).exists());
}

#[test]
fn a_carry_of_another_commit_is_refused() {
    let source = scratch();
    let dist = scratch();
    carried(source.path(), Lane::Stock);
    let mut handoff = bound(source.path());
    let err = handoff
        .import(dist.path(), VERSION, BUILD, &"b".repeat(40), &sources())
        .unwrap_err();
    assert!(err.contains("not this cut's"), "{err}");
    // The same commit, another tree: the receipt's sources are not the cut's.
    let mut moved = sources();
    moved.tree = "c".repeat(40);
    let err = handoff
        .import(dist.path(), VERSION, BUILD, &commit(), &moved)
        .unwrap_err();
    assert!(err.contains("source tree"), "{err}");
    assert!(handoff.artifact.is_none(), "nothing frozen");
    assert!(
        !dist.path().join(TARGET.asset_name(VERSION)).exists(),
        "nothing staged"
    );
}

#[test]
fn a_zip_that_is_not_the_receipts_is_refused() {
    let source = scratch();
    let dist = scratch();
    let receipt = carried(source.path(), Lane::Stock);
    let mut bytes = fixture::portable();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    fs::write(source.path().join(&receipt.asset), bytes).unwrap();
    let mut handoff = bound(source.path());
    let err = handoff
        .import(dist.path(), VERSION, BUILD, &commit(), &sources())
        .unwrap_err();
    assert!(err.contains("SHA-256"), "{err}");
    assert!(!dist.path().join(&receipt.asset).exists(), "nothing staged");
}

#[test]
fn a_missing_file_says_how_to_make_it() {
    for which in 0..2 {
        let source = scratch();
        let dist = scratch();
        let receipt = carried(source.path(), Lane::Stock);
        let gone = [receipt.asset.clone(), handoff::receipt_name(&receipt.asset)][which].clone();
        fs::remove_file(source.path().join(&gone)).unwrap();
        let mut handoff = bound(source.path());
        let err = handoff
            .import(dist.path(), VERSION, BUILD, &commit(), &sources())
            .unwrap_err();
        assert!(err.contains(&gone), "{err}");
        assert!(
            err.contains(&format!(
                "aterm-winship release-build --version {VERSION} --build-number {BUILD} \
                 --commit {}",
                commit()
            )),
            "{err}"
        );
    }
}

#[test]
fn an_extra_file_is_refused() {
    let source = scratch();
    let dist = scratch();
    carried(source.path(), Lane::Stock);
    fs::write(source.path().join("aterm-0.100.0-x64-portable.zip"), b"old").unwrap();
    let mut handoff = bound(source.path());
    let err = handoff
        .import(dist.path(), VERSION, BUILD, &commit(), &sources())
        .unwrap_err();
    assert!(err.contains("x64-portable"), "{err}");
}

#[test]
fn a_trust_the_commit_does_not_pin_is_refused() {
    let source = scratch();
    let dist = scratch();
    carried(source.path(), Lane::Trust);
    let mut handoff = bound(source.path());
    let mut unlocked = sources();
    unlocked.trust_commit = Some("d".repeat(40));
    assert!(
        handoff
            .import(dist.path(), VERSION, BUILD, &commit(), &unlocked)
            .unwrap_err()
            .contains("trust.lock")
    );
    unlocked.trust_commit = None;
    assert!(
        handoff
            .import(dist.path(), VERSION, BUILD, &commit(), &unlocked)
            .is_err()
    );
}

#[test]
fn a_resume_refuses_changed_inputs_and_tampered_staged_bytes() {
    let source = scratch();
    let dist = scratch();
    let receipt = carried(source.path(), Lane::Stock);
    let mut handoff = bound(source.path());
    handoff
        .import(dist.path(), VERSION, BUILD, &commit(), &sources())
        .unwrap();
    // Another valid build of the same cut, carried in its place.
    let commit = commit();
    let sources = sources();
    let mut other = fixture::receipt(
        &fixture::portable(),
        &cut(&commit, &sources),
        Lane::Stock,
        TRUST_COMMIT,
    );
    other.compiler_sha256 = "f".repeat(64);
    fixture::carry(source.path(), &fixture::portable(), &other).unwrap();
    assert!(
        handoff
            .clone()
            .import(dist.path(), VERSION, BUILD, &commit, &sources)
            .unwrap_err()
            .contains("changed")
    );
    let mut bytes = fs::read(dist.path().join(&receipt.asset)).unwrap();
    bytes[40] ^= 1;
    fs::write(dist.path().join(&receipt.asset), bytes).unwrap();
    assert!(
        handoff
            .verify_staged(dist.path(), VERSION, BUILD, &commit)
            .is_err()
    );
    let unfrozen = Handoff::new(source.path(), None).unwrap();
    assert!(
        unfrozen
            .verify_staged(dist.path(), VERSION, BUILD, &commit)
            .unwrap_err()
            .contains("not frozen")
    );
}

#[test]
fn the_channel_release_carries_the_frozen_zip_exactly_once() {
    let source = scratch();
    let receipt = carried(source.path(), Lane::Stock);
    let zip = std::slice::from_ref(&receipt.asset);
    let mut published = crate::channel::required_asset_names(VERSION, false, false);
    assert!(
        crate::channel::validate_channel_asset_set(&published, VERSION, false, false, zip).is_err(),
        "a frozen zip the release does not carry is missing"
    );
    published.push(receipt.asset.clone());
    crate::channel::validate_channel_asset_set(&published, VERSION, false, false, zip).unwrap();
    assert!(
        crate::channel::validate_channel_asset_set(&published, VERSION, false, false, &[]).is_err(),
        "a zip the cut did not freeze is foreign"
    );
    published.push(receipt.asset.clone());
    assert!(
        crate::channel::validate_channel_asset_set(&published, VERSION, false, false, zip).is_err(),
        "a duplicated zip is refused"
    );
    assert!(
        crate::channel::validate_channel_asset_set(
            &crate::channel::required_asset_names(VERSION, false, false),
            VERSION,
            false,
            false,
            &["aterm-0.99.0-windows-x86_64.zip".to_string()],
        )
        .is_err(),
        "another version's zip is no asset of this cut"
    );
}

#[test]
fn sources_read_the_commits_tree_and_pins() {
    let dir = scratch();
    let repo = dir.path();
    let git = |args: &[&str]| {
        let out = aterm_uds::spawnfd::command("git")
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
    };
    git(&["init", "-q"]);
    let pin = repo.join(handoff::STOCK_PIN_FILE);
    fs::create_dir_all(pin.parent().unwrap()).unwrap();
    fs::write(&pin, "pub const STOCK_PIN: &str = \"1.97.1\";\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "no lock"]);
    let unlocked = git(&["rev-parse", "HEAD"]);
    let runner = crate::ledger::GitCli::new(repo);
    let read = Sources::at(&runner, &unlocked).unwrap();
    assert_eq!(read.tree, git(&["rev-parse", "HEAD^{tree}"]));
    assert_eq!((read.stock.as_str(), read.trust_commit), ("1.97.1", None));
    fs::write(
        repo.join("trust.lock"),
        format!(
            "version = 1\n\n[trust]\nbuild = 16387\ntree_root = \"{}\"\ncompiler_commit = \
             \"{TRUST_COMMIT}\"\nreceipt = \"SEED-MINT sha256:{}\"\n\n[stock]\ntoolchain = \
             \"1.99.1\"\n",
            "4".repeat(64),
            "5".repeat(64)
        ),
    )
    .unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "lock"]);
    let locked = git(&["rev-parse", "HEAD"]);
    let read = Sources::at(&runner, &locked).unwrap();
    assert_eq!(read.trust_commit.as_deref(), Some(TRUST_COMMIT));
    // Read at the commit, not the work tree.
    fs::write(&pin, "pub const STOCK_PIN: &str = \"1.98.0\";\n").unwrap();
    assert_eq!(Sources::at(&runner, &unlocked).unwrap().stock, "1.97.1");
    assert!(Sources::at(&runner, &"f".repeat(40)).is_err());
}

#[test]
fn the_windows_declaration_is_optional_once_and_travels_with_the_release() {
    use crate::cli::{Cmd, MAC_ONLY_REFUSAL, parse};
    let parse = |args: &[&str]| parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    let dir = std::path::PathBuf::from("/private/windows");
    match parse(&[
        "cut",
        "--mac-only",
        "--windows-artifacts",
        "/private/windows",
    ]) {
        Ok(Cmd::Cut { opts, .. }) => assert_eq!(opts.windows_artifacts, Some(dir.clone())),
        other => panic!("{other:?}"),
    }
    match parse(&["cut", "--sign", "--windows-artifacts", "/private/windows"]) {
        Ok(Cmd::Cut { opts, .. }) => assert_eq!(opts.windows_artifacts, Some(dir.clone())),
        other => panic!("{other:?}"),
    }
    // The Windows zip is no Linux declaration: a real cut still owes one.
    assert_eq!(
        parse(&["cut", "--windows-artifacts", "/private/windows"]).unwrap_err(),
        MAC_ONLY_REFUSAL
    );
    for refused in [
        &["cut", "--mac-only", "--windows-artifacts"][..],
        &["cut", "--mac-only", "--windows-artifacts", "--dry-run"],
        &[
            "cut",
            "--mac-only",
            "--windows-artifacts",
            "/a",
            "--windows-artifacts",
            "/b",
        ],
        &["cut", "--resume", "--windows-artifacts", "/changed"],
        &["cut", "--abandon", "v0.100.0", "--windows-artifacts", "/w"],
        &["release", "--prefreeze", "--windows-artifacts", "/w"],
        &["release", "--forget", "--windows-artifacts", "/w"],
        &[
            "release",
            "--abandon",
            "v0.100.0",
            "--windows-artifacts",
            "/w",
        ],
        &[
            "release",
            "--windows-artifacts",
            "/a",
            "--windows-artifacts",
            "/b",
        ],
    ] {
        assert!(parse(refused).is_err(), "{refused:?}");
    }
    match parse(&[
        "yank",
        "7",
        "--mac-only",
        "--windows-artifacts",
        "/private/windows",
    ]) {
        Ok(Cmd::Yank { opts, .. }) => assert_eq!(opts.windows_artifacts, Some(dir.clone())),
        other => panic!("{other:?}"),
    }
    let Ok(Cmd::Release(opts)) =
        parse(&["release", "--mac-only", "--windows-artifacts", "handoff"])
    else {
        panic!("release parses");
    };
    // Anchored where the release started, and handed on so to R's cutter.
    let start = std::path::Path::new("/started/here");
    let anchored = opts.anchored(start);
    assert_eq!(anchored.windows, Some(start.join("handoff")));
    let args: Vec<std::ffi::OsString> = ["release", "--mac-only", "--windows-artifacts", "handoff"]
        .iter()
        .map(Into::into)
        .collect();
    let want: Vec<std::ffi::OsString> = vec![
        "release".into(),
        "--mac-only".into(),
        "--windows-artifacts".into(),
        start.join("handoff").into_os_string(),
    ];
    assert_eq!(anchored.exec_args(args), want);
}

#[test]
fn a_journal_without_windows_keeps_its_bytes() {
    let journal = crate::release::Journal {
        version: VERSION.to_string(),
        commit: commit(),
        ..crate::release::Journal::default()
    };
    let text = aterm_toml::to_string(&journal).unwrap();
    assert!(!text.contains("windows"), "{text}");
    let declared = crate::release::Journal {
        windows: Some(std::path::PathBuf::from("/private/windows")),
        ..journal
    };
    let text = aterm_toml::to_string(&declared).unwrap();
    let back: crate::release::Journal = aterm_toml::from_str(&text).unwrap();
    assert_eq!(back, declared);
}

#[test]
fn a_carry_is_admitted_only_by_the_digest_named_on_the_windows_host() {
    let commit = commit();
    let sources = sources();
    let genuine = fixture::sha256(&fixture::portable());
    // A pair written by whoever can write where the carry travels: a zip of its own and a
    // receipt that agrees with it and with this cut, which admission alone takes.
    let forged_zip = fixture::zip(&[
        ("aterm/BUILD.json", b"{\"forged\":1}".to_vec()),
        ("aterm/OpenConsole.exe", fixture::image(3)),
        (handoff::VERIFIER_MEMBER, fixture::verifier()),
        ("aterm/aterm-gui.exe", fixture::image(2)),
        ("aterm/aterm.exe", fixture::image(3)),
    ]);
    let forged = fixture::receipt(
        &forged_zip,
        &cut(&commit, &sources),
        Lane::Stock,
        TRUST_COMMIT,
    );
    let source = scratch();
    fixture::carry(source.path(), &forged_zip, &forged).unwrap();
    for (named, words) in [
        (None, "--windows-sha256"),
        (Some(genuine.as_str()), genuine.as_str()),
    ] {
        let dist = scratch();
        let mut handoff = Handoff::new(source.path(), named).unwrap();
        let err = handoff
            .import(dist.path(), VERSION, BUILD, &commit, &sources)
            .expect_err("a pair the Windows host did not print was admitted");
        assert!(err.contains(words), "{err}");
        assert!(handoff.artifact.is_none(), "nothing frozen");
        assert!(!dist.path().join(&forged.asset).exists(), "nothing staged");
    }
    // Named as the Windows host printed it (in either case), the pair is admitted.
    let dist = scratch();
    let mut handoff = Handoff::new(source.path(), Some(&forged.sha256.to_uppercase())).unwrap();
    assert_eq!(handoff.sha256.as_deref(), Some(forged.sha256.as_str()));
    handoff
        .import(dist.path(), VERSION, BUILD, &commit, &sources)
        .unwrap();
    assert!(Handoff::new(source.path(), Some("not-a-digest")).is_err());
}

#[test]
fn a_frozen_zip_stays_bound_to_the_digest_it_was_admitted_by() {
    let source = scratch();
    let dist = scratch();
    carried(source.path(), Lane::Stock);
    let mut handoff = bound(source.path());
    handoff
        .import(dist.path(), VERSION, BUILD, &commit(), &sources())
        .unwrap();
    handoff
        .verify_staged(dist.path(), VERSION, BUILD, &commit())
        .unwrap();
    // A journal that lost the digest, or names another, no longer holds the zip it froze.
    for named in [None, Some("e".repeat(64))] {
        let mut changed = handoff.clone();
        changed.sha256 = named;
        assert!(
            changed
                .verify_staged(dist.path(), VERSION, BUILD, &commit())
                .is_err()
        );
    }
    let mut malformed = handoff.clone();
    malformed.sha256 = Some("E".repeat(64));
    assert!(
        malformed
            .validate_identity(VERSION, BUILD, &commit(), false)
            .unwrap_err()
            .contains("no SHA-256")
    );
    // A handoff declared without one writes no key.
    let unbound = Handoff::new(source.path(), None).unwrap();
    assert!(!aterm_toml::to_string(&unbound).unwrap().contains("sha256"));
}

#[test]
fn the_windows_digest_is_named_once_in_lowercase_and_travels_with_the_release() {
    use crate::cli::{Cmd, parse};
    let parse = |args: &[&str]| parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    let upper = "AB".repeat(32);
    let lower = "ab".repeat(32);
    match parse(&[
        "cut",
        "--mac-only",
        "--windows-artifacts",
        "/w",
        "--windows-sha256",
        &upper,
    ]) {
        Ok(Cmd::Cut { opts, .. }) => assert_eq!(opts.windows_sha256, Some(lower.clone())),
        other => panic!("{other:?}"),
    }
    match parse(&[
        "yank",
        "7",
        "--mac-only",
        "--windows-artifacts",
        "/w",
        "--windows-sha256",
        &lower,
    ]) {
        Ok(Cmd::Yank { opts, .. }) => assert_eq!(opts.windows_sha256, Some(lower.clone())),
        other => panic!("{other:?}"),
    }
    // A release may name it alone, on the run after the cut stopped for the pair: its journal
    // holds the directory.
    let Ok(Cmd::Release(opts)) = parse(&["release", "--windows-sha256", &upper]) else {
        panic!("release parses");
    };
    assert_eq!(opts.windows_sha256, Some(lower.clone()));
    for refused in [
        &["cut", "--mac-only", "--windows-sha256", &lower][..],
        &[
            "cut",
            "--mac-only",
            "--windows-artifacts",
            "/w",
            "--windows-sha256",
        ],
        &[
            "cut",
            "--mac-only",
            "--windows-artifacts",
            "/w",
            "--windows-sha256",
            "abc",
        ],
        &[
            "cut",
            "--mac-only",
            "--windows-artifacts",
            "/w",
            "--windows-sha256",
            &lower,
            "--windows-sha256",
            &lower,
        ],
        &["cut", "--resume", "--windows-sha256", &lower],
        &["cut", "--abandon", "v0.100.0", "--windows-sha256", &lower],
        &["yank", "7", "--mac-only", "--windows-sha256", &lower],
        &["release", "--prefreeze", "--windows-sha256", &lower],
        &["release", "--forget", "--windows-sha256", &lower],
        &[
            "release",
            "--abandon",
            "v0.100.0",
            "--windows-sha256",
            &lower,
        ],
        &["release", "--windows-sha256", &"g".repeat(64)],
    ] {
        assert!(parse(refused).is_err(), "{refused:?}");
    }
    // Handed on unchanged to R's cutter, and journaled; a journal naming none writes no key.
    let args: Vec<std::ffi::OsString> = ["release", "--windows-sha256", lower.as_str()]
        .iter()
        .map(Into::into)
        .collect();
    assert_eq!(opts.exec_args(args.clone()), args);
    let journal = crate::release::Journal {
        version: VERSION.to_string(),
        commit: commit(),
        windows: Some(std::path::PathBuf::from("/private/windows")),
        ..crate::release::Journal::default()
    };
    assert!(
        !aterm_toml::to_string(&journal)
            .unwrap()
            .contains("windows_sha256")
    );
    let named = crate::release::Journal {
        windows_sha256: Some(lower),
        ..journal
    };
    let back: crate::release::Journal =
        aterm_toml::from_str(&aterm_toml::to_string(&named).unwrap()).unwrap();
    assert_eq!(back, named);
}
