// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// One change to a fixture.
pub(crate) type Change<T> = Box<dyn Fn(&mut T)>;

pub(crate) const VERSION: &str = "0.200.0";
pub(crate) const BUILD: u64 = 1_794_421_328;
pub(crate) const COMMIT: &str = "1a37de31a3401234567890abcdef1234567890ab";
pub(crate) const TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
pub(crate) const TRUST_COMMIT: &str = "29ebb403de7a102850d6cb10a62d5cc3baf723ed";

pub(crate) use super::fixture::{STOCK_VV, trust_vv};
use super::fixture::{image, portable, sha256, zip};

pub(crate) const PINS: Pins<'static> = Pins {
    stock: super::fixture::STOCK,
    trust_commit: Some(TRUST_COMMIT),
};

pub(crate) const CUT: Cut<'static> = Cut {
    version: VERSION,
    build: BUILD,
    commit: COMMIT,
    source_tree: TREE,
};

/// The receipt `release-build` writes for `zip` of [`CUT`] in `lane`.
pub(crate) fn receipt(zip: &[u8], lane: Lane) -> Receipt {
    super::fixture::receipt(zip, &CUT, lane, TRUST_COMMIT)
}

/// A handoff directory holding `zip` and `receipt` under their canonical names.
fn carry(zip: &[u8], receipt: &Receipt) -> aterm_tempfile::TempDir {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    super::fixture::carry(dir.path(), zip, receipt).expect("carry");
    dir
}

#[test]
fn a_carry_of_the_cut_is_admitted_in_either_pinned_lane() {
    let bytes = portable();
    for lane in [Lane::Stock, Lane::Trust] {
        let want = receipt(&bytes, lane);
        let dir = carry(&bytes, &want);
        assert_eq!(admit(dir.path(), &CUT, &PINS), Ok(want), "{lane:?}");
    }
}

#[test]
fn a_receipt_round_trips_its_toml_and_refuses_an_unknown_key() {
    let want = receipt(&portable(), Lane::Stock);
    let text = want.to_toml().expect("toml");
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("r.toml");
    std::fs::write(&path, &text).expect("write");
    assert_eq!(read_receipt(&path), Ok(want));
    std::fs::write(&path, format!("{text}extra = 1\n")).expect("write");
    assert!(read_receipt(&path).is_err(), "an unknown key was read");
    std::fs::write(&path, " ".repeat(RECEIPT_MAX_BYTES as usize + 1)).expect("write");
    assert!(
        read_receipt(&path).is_err(),
        "an oversized receipt was read"
    );
}

#[test]
fn a_receipt_of_another_cut_is_refused() {
    let bytes = portable();
    let good = receipt(&bytes, Lane::Stock);
    let other = "0123456789abcdef0123456789abcdef01234567";
    let cases: [(&str, Change<Receipt>); 7] = [
        ("commit", Box::new(|r| r.commit = other.to_string())),
        ("build", Box::new(|r| r.build_number = BUILD + 1)),
        ("version", Box::new(|r| r.version = "0.199.0".to_string())),
        ("schema", Box::new(|r| r.schema = SCHEMA + 1)),
        (
            "target",
            Box::new(|r| r.target = "aarch64-pc-windows-msvc".to_string()),
        ),
        ("lane", Box::new(|r| r.lane = "nightly".to_string())),
        ("digest", Box::new(|r| r.compiler_sha256 = "F".repeat(64))),
    ];
    for (what, change) in cases {
        let mut bad = good.clone();
        change(&mut bad);
        let dir = carry(&bytes, &bad);
        assert!(
            admit(dir.path(), &CUT, &PINS).is_err(),
            "a receipt with another {what} was admitted"
        );
    }
}

#[test]
fn a_zip_that_is_not_the_receipts_is_refused() {
    let bytes = portable();
    let good = receipt(&bytes, Lane::Stock);
    // The receipt names other bytes' digest.
    let mut wrong_sha = good.clone();
    wrong_sha.sha256 = sha256(b"other bytes");
    let dir = carry(&bytes, &wrong_sha);
    let err = admit(dir.path(), &CUT, &PINS).expect_err("a wrong digest was admitted");
    assert!(err.contains("SHA-256"), "{err}");
    // The bytes changed under the receipt, the size kept.
    let mut flipped = bytes.clone();
    let last = flipped.len() - 1;
    flipped[last] ^= 1;
    let dir = carry(&flipped, &good);
    assert!(
        admit(dir.path(), &CUT, &PINS).is_err(),
        "changed bytes were admitted"
    );
    // The receipt names another size.
    let mut wrong_size = good.clone();
    wrong_size.size += 1;
    let dir = carry(&bytes, &wrong_size);
    assert!(
        admit(dir.path(), &CUT, &PINS).is_err(),
        "a wrong size was admitted"
    );
}

#[test]
fn a_zip_of_the_receipts_digest_is_still_held_to_what_a_windows_release_is() {
    // Digest and size agree, but the archive has no windowed image.
    let bytes = zip(&[
        ("aterm/aterm.exe", image(3)),
        ("aterm/README.txt", b"x".repeat(64)),
    ]);
    let dir = carry(&bytes, &receipt(&bytes, Lane::Stock));
    let err = admit(dir.path(), &CUT, &PINS).expect_err("a zip with no aterm-gui.exe was admitted");
    assert!(err.contains("aterm-gui.exe"), "{err}");
}

#[test]
fn a_release_zip_carries_a_console_verifier() {
    let images = || {
        vec![
            ("aterm/aterm-gui.exe", image(2)),
            ("aterm/aterm.exe", image(3)),
        ]
    };
    // A zip that is a Windows release but carries no verifier: get-aterm.ps1 would have
    // none to pin.
    let bytes = zip(&images());
    let dir = carry(&bytes, &receipt(&bytes, Lane::Stock));
    let err = admit(dir.path(), &CUT, &PINS).expect_err("a zip with no verifier was admitted");
    assert!(err.contains(VERIFIER_MEMBER), "{err}");
    // One whose verifier is a windowed image: get-aterm.ps1 reads its standard output.
    let mut members = images();
    members.push((VERIFIER_MEMBER, image(2)));
    let bytes = zip(&members);
    let dir = carry(&bytes, &receipt(&bytes, Lane::Stock));
    let err = admit(dir.path(), &CUT, &PINS).expect_err("a windowed verifier was admitted");
    assert!(err.contains(VERIFIER_MEMBER), "{err}");
    // The fixture's own carries one, which reads back as its bytes.
    assert_eq!(
        zip_verifier(std::io::Cursor::new(portable())),
        Ok(super::fixture::verifier())
    );
}

#[test]
fn a_carry_missing_either_file_names_what_it_lacks() {
    let bytes = portable();
    let good = receipt(&bytes, Lane::Stock);
    let receipt_file = receipt_name(&good.asset);
    for gone in [good.asset.as_str(), receipt_file.as_str()] {
        let dir = carry(&bytes, &good);
        std::fs::remove_file(dir.path().join(gone)).expect("remove");
        assert_eq!(missing(dir.path(), VERSION), Ok(vec![gone.to_string()]));
        let err = admit(dir.path(), &CUT, &PINS).expect_err("a carry missing a file was admitted");
        assert!(err.contains(gone), "{err}");
    }
    let empty = aterm_tempfile::tempdir().expect("tempdir");
    assert_eq!(
        missing(empty.path(), VERSION),
        Ok(vec![good.asset.clone(), receipt_file])
    );
}

#[test]
fn a_carry_with_another_file_is_refused_and_a_dot_file_is_passed_over() {
    let bytes = portable();
    let good = receipt(&bytes, Lane::Stock);
    let dir = carry(&bytes, &good);
    std::fs::write(dir.path().join(".DS_Store"), b"finder").expect("dot file");
    std::fs::create_dir(dir.path().join(".building")).expect("dot folder");
    assert_eq!(admit(dir.path(), &CUT, &PINS), Ok(good.clone()));
    for extra in [
        "aterm-0.200.0-x64-portable.zip",
        "aterm-0.199.0-windows-x86_64.zip",
        "notes.txt",
    ] {
        let dir = carry(&bytes, &good);
        std::fs::write(dir.path().join(extra), b"extra").expect("extra");
        let err = admit(dir.path(), &CUT, &PINS).expect_err("an extra file was admitted");
        assert!(err.contains(extra), "{err}");
    }
    let dir = carry(&bytes, &good);
    std::fs::create_dir(dir.path().join("aterm")).expect("folder");
    assert!(
        admit(dir.path(), &CUT, &PINS).is_err(),
        "an extra folder was admitted"
    );
}

#[test]
fn a_receipt_of_another_tree_is_refused() {
    let bytes = portable();
    let mut bad = receipt(&bytes, Lane::Stock);
    bad.source_tree = "5".repeat(40);
    let dir = carry(&bytes, &bad);
    let err = admit(dir.path(), &CUT, &PINS).expect_err("another tree was admitted");
    assert!(err.contains("source tree"), "{err}");
}

#[test]
fn only_the_pinned_compilers_are_admitted() {
    let ok = |lane, vv: &str| check_compiler(lane, vv, &PINS);
    assert_eq!(ok(Lane::Stock, STOCK_VV), Ok(()));
    assert_eq!(ok(Lane::Trust, &trust_vv(TRUST_COMMIT)), Ok(()));
    // A Trust build the lock does not pin: in neither lane.
    let unpinned = trust_vv("dbc6adef214925852160bf2113a24fdfd18832e9");
    assert!(ok(Lane::Trust, &unpinned).is_err());
    assert!(ok(Lane::Stock, &unpinned).is_err());
    // A stock release that is not the pin, and a cross or foreign host.
    assert!(
        ok(
            Lane::Stock,
            &STOCK_VV.replace("release: 1.97.1", "release: 1.99.1")
        )
        .is_err()
    );
    assert!(
        ok(
            Lane::Stock,
            &STOCK_VV.replace("x86_64-pc-windows-msvc", "x86_64-pc-windows-gnu")
        )
        .is_err()
    );
    assert!(
        ok(
            Lane::Stock,
            &STOCK_VV.replace("x86_64-pc-windows-msvc", "aarch64-apple-darwin")
        )
        .is_err()
    );
    // A stock compiler is no Trust build, and a commit with no lock pins no Trust.
    assert!(ok(Lane::Trust, STOCK_VV).is_err());
    let no_lock = Pins {
        trust_commit: None,
        ..PINS
    };
    assert!(check_compiler(Lane::Trust, &trust_vv(TRUST_COMMIT), &no_lock).is_err());
    // A -vV that says a key twice says nothing.
    let twice = format!("{STOCK_VV}\nrelease: 1.97.1");
    assert!(ok(Lane::Stock, &twice).is_err());
    // Through the receipt: an unpinned Trust carry is refused.
    let bytes = portable();
    let mut bad = receipt(&bytes, Lane::Trust);
    bad.compiler_version = unpinned;
    let dir = carry(&bytes, &bad);
    let err = admit(dir.path(), &CUT, &PINS).expect_err("an unpinned Trust was admitted");
    assert!(err.contains("trust.lock"), "{err}");
}

#[test]
fn the_stock_pin_is_read_as_build_ps1_reads_it() {
    let real = "//! [`STOCK_PIN`], in a file of its own.\n\n/// The one stock toolchain.\n\
                pub const STOCK_PIN: &str = \"1.97.1\";\n";
    assert_eq!(stock_pin(real), Some("1.97.1"));
    assert_eq!(
        stock_pin("    pub const STOCK_PIN: &str = \"1.97.1\"; // why\n"),
        Some("1.97.1")
    );
    for bad in [
        "",
        "pub const STOCK_PIN: &str = \"\";",
        "pub const STOCK_PIN: &str = \"1.97.1\"",
        "pub const STOCK_PIN: &str = \"1.97.1\";\npub const STOCK_PIN: &str = \"1.98.0\";",
        "pub const OTHER_PIN: &str = \"1.97.1\";",
    ] {
        assert_eq!(stock_pin(bad), None, "{bad:?}");
    }
}

#[test]
fn a_carry_is_bound_only_by_the_digest_read_off_the_windows_host() {
    let bytes = portable();
    let good = receipt(&bytes, Lane::Stock);
    // A forged pair agrees with itself and the cut: admission alone takes it.
    let forged_bytes = zip(&[
        ("aterm/BUILD.json", b"{\"forged\":1}".to_vec()),
        ("aterm/OpenConsole.exe", image(3)),
        (VERIFIER_MEMBER, image(3)),
        ("aterm/aterm-gui.exe", image(2)),
        ("aterm/aterm.exe", image(3)),
    ]);
    let forged = receipt(&forged_bytes, Lane::Stock);
    let dir = carry(&forged_bytes, &forged);
    let admitted = admit(dir.path(), &CUT, &PINS).expect("a self-consistent pair is admitted");
    // The digest the person typed is the genuine zip's: the forgery is refused.
    let err = admitted
        .check_carried(Some(&good.sha256))
        .expect_err("a forged pair was bound");
    assert!(
        err.contains(&good.sha256) && err.contains(&forged.sha256),
        "{err}"
    );
    // No digest named: refused, saying where to read one.
    let err = admitted
        .check_carried(None)
        .expect_err("an unnamed carry was bound");
    assert!(err.contains("--windows-sha256"), "{err}");
    assert_eq!(good.check_carried(Some(&good.sha256)), Ok(()));
    // Typed in either case, it is read as the lowercase digest; anything else is refused.
    assert_eq!(
        carried_sha256(&good.sha256.to_uppercase()),
        Ok(good.sha256.clone())
    );
    for bad in [
        "",
        "abc",
        &good.sha256[1..],
        &format!("{} ", good.sha256),
        &"g".repeat(64),
    ] {
        assert!(carried_sha256(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn lanes_are_the_words_build_ps1_takes() {
    for lane in [Lane::Stock, Lane::Trust] {
        assert_eq!(Lane::parse(lane.word()), Some(lane));
    }
    assert_eq!(Lane::parse("auto"), None);
    assert_eq!(receipt_name("a.zip"), "a.zip.handoff.toml");
    assert_eq!(
        TARGET.asset_name(VERSION),
        "aterm-0.200.0-windows-x86_64.zip"
    );
}
