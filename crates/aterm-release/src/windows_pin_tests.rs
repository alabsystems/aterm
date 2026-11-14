// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const LEDGER: &str = "# ledger\n99 0.99.0\n100 0.100.0\n";

/// An appcast of v0.100.0 with no Windows verifier.
fn manifest() -> Manifest {
    Manifest::parse(
        "schema = 1\nversion = \"0.100.0\"\nbuild_number = 100\n\
         commit = \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n\
         dmg = \"aterm-0.100.0.dmg\"\nsha256 = \"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\"\n",
    )
    .expect("an appcast")
}

/// A PE32+ x86-64 console executable's header, `len` bytes long.
fn image(len: usize) -> Vec<u8> {
    let mut bytes = vec![0u8; len.max(0x80 + 24 + 240)];
    bytes[..2].copy_from_slice(b"MZ");
    bytes[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
    bytes[0x80..0x84].copy_from_slice(b"PE\0\0");
    bytes[0x84..0x86].copy_from_slice(&0x8664u16.to_le_bytes());
    bytes[0x94..0x96].copy_from_slice(&240u16.to_le_bytes());
    bytes[0x96..0x98].copy_from_slice(&0x0022u16.to_le_bytes());
    bytes[0x98..0x9a].copy_from_slice(&0x20bu16.to_le_bytes());
    bytes[0xdc..0xde].copy_from_slice(&3u16.to_le_bytes());
    bytes
}

fn receipt(bytes: &[u8]) -> pin::Receipt {
    pin::Receipt {
        arch: "x86_64".into(),
        asset: pin::windows_asset_name("0.100.0", "x86_64"),
        sha256: crate::publish::sha256_bytes(bytes),
        size: bytes.len() as u64,
    }
}

/// The cut's stamp is the reader's rule: an accepted receipt reads back as itself with the
/// interface this tree's get-aterm.ps1 calls and round-trips through the emitted appcast; one
/// the reader would refuse changes nothing.
#[test]
fn a_cut_stamps_its_windows_verifier_only_when_it_reads_back() {
    let good = receipt(&image(4096));
    let mut m = manifest();
    assert_eq!(signed_windows_verifiers(&m), Ok(None));
    let before = m.clone();
    let bent = |f: &dyn Fn(&mut pin::Receipt)| {
        let mut bad = good.clone();
        f(&mut bad);
        bad
    };
    for bad in [
        bent(&|r| r.arch = "aarch64".into()),
        bent(&|r| r.asset = pin::windows_asset_name("0.99.0", "x86_64")),
        bent(&|r| r.asset = pin::asset_name("0.100.0", "x86_64")),
        bent(&|r| r.sha256 = r.sha256.to_uppercase()),
        bent(&|r| r.size = 63),
        bent(&|r| r.size = pin::MAX_ASSET + 1),
    ] {
        assert!(stamp(&mut m, &bad).is_err(), "{bad:?}");
        assert_eq!(m, before, "{bad:?}");
    }
    stamp(&mut m, &good).expect("the stamp");
    assert_eq!(
        signed_windows_verifiers(&m),
        Ok(Some((vec![good.clone()], pin::WINDOWS_INTERFACE)))
    );
    let text = crate::manifest_out::emit(&m).expect("the emitted appcast");
    assert!(text.contains(&format!("windows_verifier_x86_64 = \"{}\"\n", good.asset)));
    assert_eq!(
        signed_windows_verifiers(&Manifest::parse(&text).expect("it parses")),
        Ok(Some((vec![good], pin::WINDOWS_INTERFACE)))
    );
}

/// What a release serves is held to the signed receipt: its size, its digest, and an x86-64
/// console executable.
#[test]
fn a_served_windows_verifier_is_held_to_its_signed_receipt() {
    let dir = aterm_tempfile::tempdir().expect("a scratch folder");
    let at = dir.path().join("verifier.exe");
    let bytes = image(4096);
    std::fs::write(&at, &bytes).expect("the verifier");
    let good = receipt(&bytes);
    assert_eq!(windows_verifier_file(&at, good.size, &good.sha256), Ok(()));
    for (size, sha256, why) in [
        (good.size + 1, good.sha256.clone(), "of the signed size"),
        (good.size, "f".repeat(64), "of the signed SHA-256"),
    ] {
        let said = windows_verifier_file(&at, size, &sha256).expect_err(why);
        assert!(said.contains(why), "{why}: {said}");
    }
    let mut elf = vec![0u8; 4096];
    elf[..4].copy_from_slice(b"\x7fELF");
    std::fs::write(&at, &elf).expect("an ELF");
    let elf_receipt = receipt(&elf);
    let said = windows_verifier_file(&at, elf_receipt.size, &elf_receipt.sha256)
        .expect_err("an ELF is no Windows verifier");
    assert!(said.contains("no MZ header"), "{said}");
}

/// The pin and its record are written together from a claimed record; an unclaimed one writes
/// neither.
#[test]
fn the_windows_pin_is_written_with_its_record_or_not_at_all() {
    let root = aterm_tempfile::tempdir().expect("a checkout");
    let script = format!("# get-aterm.ps1\n{}rest\n", WindowsPin::default().lines());
    let script_at = root.path().join(pin::WINDOWS_SCRIPT);
    std::fs::create_dir_all(script_at.parent().expect("apps/aterm-win")).expect("a folder");
    std::fs::create_dir_all(root.path().join("crates/aterm-bootstrap-verify")).expect("a folder");
    std::fs::write(&script_at, &script).expect("get-aterm.ps1");
    let record = WindowsRecord(pin::Record {
        repo: "example/aterm".into(),
        tag: "v0.100.0".into(),
        build_number: 100,
        commit: "a".repeat(40),
        interface: pin::WINDOWS_INTERFACE,
        verifiers: vec![receipt(&image(4096))],
    });
    std::fs::write(root.path().join("RELEASES.ledger"), "99 0.99.0\n").expect("a ledger");
    let said = write_windows_pin(root.path(), &record).expect_err("an unclaimed release");
    assert!(said.contains("records no claim"), "{said}");
    assert_eq!(
        std::fs::read_to_string(&script_at).expect("get-aterm.ps1"),
        script
    );
    assert!(!root.path().join(pin::WINDOWS_RECORD).exists());
    std::fs::write(root.path().join("RELEASES.ledger"), LEDGER).expect("a ledger");
    assert_eq!(write_windows_pin(root.path(), &record), Ok(record.pin()));
    let written = std::fs::read_to_string(&script_at).expect("get-aterm.ps1");
    let committed =
        std::fs::read_to_string(root.path().join(pin::WINDOWS_RECORD)).expect("the record");
    assert_eq!(
        pin::check_windows(&written, Some(&committed), LEDGER),
        Ok(record.pin())
    );
}
