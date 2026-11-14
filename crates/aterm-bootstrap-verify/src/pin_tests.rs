// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn record() -> Record {
    Record {
        repo: "alabsystems/aterm".into(),
        tag: "v0.119.0".into(),
        build_number: 1_792_732_523,
        commit: "9d7503dd481e7cd23a17270799e522441b758648".into(),
        interface: INTERFACE,
        verifiers: vec![
            Receipt {
                arch: "x86_64".into(),
                asset: "aterm-bootstrap-verify-0.119.0-linux-x86_64".into(),
                sha256: "a".repeat(64),
                size: 2_000_000,
            },
            Receipt {
                arch: "aarch64".into(),
                asset: "aterm-bootstrap-verify-0.119.0-linux-aarch64".into(),
                sha256: "b".repeat(64),
                size: 1_900_000,
            },
        ],
    }
}

const LEDGER: &str = "# the ledger\n1792728567 0.118.0\n1792732523 0.119.0\n";

#[test]
fn a_pin_reads_back_as_it_was_written() {
    let pin = record().pin();
    let script = format!("#!/bin/bash\n# a comment\n{}more\n", pin.lines());
    assert_eq!(Pin::read(&script), Ok(pin.clone()));
    assert_eq!(Pin::read(&Pin::default().lines()), Ok(Pin::default()));
    let empty = format!("#!/bin/bash\n# a comment\n{}more\n", Pin::default().lines());
    assert_eq!(pin.rewrite(&empty), Ok(script.clone()));
    assert_eq!(Pin::default().rewrite(&script), Ok(empty.clone()));
    assert_eq!(pin.rewrite(&script), Ok(script.clone()));
    assert_eq!(
        asset_name("0.119.0", "x86_64"),
        "aterm-bootstrap-verify-0.119.0-linux-x86_64"
    );
    assert_eq!(
        static_build_config("aarch64-unknown-linux-gnu"),
        "target.aarch64-unknown-linux-gnu.rustflags=[\"-C\", \"target-feature=+crt-static\"]"
    );
}

#[test]
fn a_pin_of_another_shape_is_refused() {
    let good = record().pin();
    for bad in [
        Pin {
            tag: "0.119.0".into(),
            ..good.clone()
        },
        Pin {
            tag: "v0.119.00".into(),
            ..good.clone()
        },
        Pin {
            x86_64: "A".repeat(64),
            ..good.clone()
        },
        Pin {
            aarch64: "b".repeat(63),
            ..good.clone()
        },
        Pin {
            x86_64: String::new(),
            aarch64: String::new(),
            ..good.clone()
        },
        Pin {
            tag: String::new(),
            ..good.clone()
        },
        Pin {
            repo: String::new(),
            ..good.clone()
        },
        Pin {
            repo: "alabsystems/aterm/x".into(),
            ..good.clone()
        },
        Pin {
            repo: "../aterm".into(),
            ..good.clone()
        },
    ] {
        assert!(bad.validate().is_err(), "{bad:?}");
        assert!(Pin::read(&bad.lines()).is_err(), "{bad:?}");
    }
    let twice = format!("{}{}", good.lines(), good.lines());
    assert!(Pin::read(&twice).is_err(), "a key set twice");
    assert!(good.rewrite(&twice).is_err(), "a pin rewritten twice");
    assert!(Pin::read("LINUX_VERIFIER_TAG=\"$(id)\"\n").is_err());
    let apart = good.lines().replacen('\n', "\n# between\n", 1);
    assert!(Pin::read(&apart).is_ok());
    assert!(good.rewrite(&apart).is_err(), "the four lines apart");
}

#[test]
fn a_record_reads_back_and_another_shape_is_refused() {
    let good = record();
    assert_eq!(Record::parse(&good.to_toml()), Ok(good.clone()));
    let one = Record {
        verifiers: good.verifiers[1..].to_vec(),
        ..good.clone()
    };
    assert_eq!(Record::parse(&one.to_toml()), Ok(one.clone()));
    assert_eq!(one.pin().x86_64, "");
    let receipt = |n: usize, f: &dyn Fn(&mut Receipt)| {
        let mut bad = good.clone();
        f(&mut bad.verifiers[n]);
        bad
    };
    for bad in [
        Record {
            build_number: 0,
            ..good.clone()
        },
        Record {
            commit: "9d7503dd48".into(),
            ..good.clone()
        },
        Record {
            interface: 0,
            ..good.clone()
        },
        Record {
            verifiers: Vec::new(),
            ..good.clone()
        },
        Record {
            verifiers: vec![good.verifiers[1].clone(), good.verifiers[0].clone()],
            ..good.clone()
        },
        Record {
            verifiers: vec![good.verifiers[0].clone(), good.verifiers[0].clone()],
            ..good.clone()
        },
        Record {
            repo: "aterm".into(),
            ..good.clone()
        },
        receipt(0, &|r| {
            r.asset = "aterm-bootstrap-verify-0.118.0-linux-x86_64".into()
        }),
        receipt(0, &|r| r.arch = "riscv64".into()),
        receipt(1, &|r| r.sha256 = "B".repeat(64)),
        receipt(1, &|r| r.size = 0),
        receipt(1, &|r| r.size = MAX_ASSET + 1),
    ] {
        assert!(bad.validate().is_err(), "{bad:?}");
        assert!(Record::parse(&bad.to_toml()).is_err(), "{bad:?}");
    }
    let text = good.to_toml();
    for (from, to) in [
        ("schema = 1", "schema = 2"),
        ("interface = 1", "interface = \"1\""),
        ("size = 2000000", "size = -1"),
        ("size = 2000000", "size = 2000000\nsigned = true"),
        ("interface = 1", "interface = 1\nnote = \"\""),
    ] {
        assert!(Record::parse(&text.replacen(from, to, 1)).is_err(), "{to}");
    }
    assert!(Record::parse("not = [toml").is_err());
}

#[test]
fn a_pin_is_held_to_its_record_and_the_ledger() {
    let good = record();
    let pinned = format!("#!/bin/bash\n{}", good.pin().lines());
    let text = good.to_toml();
    assert_eq!(check(&pinned, Some(&text), LEDGER), Ok(good.pin()));
    let empty = format!("#!/bin/bash\n{}", Pin::default().lines());
    assert_eq!(check(&empty, None, LEDGER), Ok(Pin::default()));
    let mistyped = Pin {
        x86_64: format!("{}c", &good.pin().x86_64[..63]),
        ..good.pin()
    };
    let other_repo = Pin {
        repo: "my-org/aterm".into(),
        ..good.pin()
    };
    for (script, record, ledger, why) in [
        (pinned.clone(), None, LEDGER, "is missing"),
        (empty.clone(), Some(text.clone()), LEDGER, "pins nothing"),
        (
            format!("#!/bin/bash\n{}", mistyped.lines()),
            Some(text.clone()),
            LEDGER,
            "is not the one",
        ),
        (
            format!("#!/bin/bash\n{}", other_repo.lines()),
            Some(text.clone()),
            LEDGER,
            "is not the one",
        ),
        (
            pinned.clone(),
            Some(text.clone()),
            "1792728567 0.118.0\n1792732524 0.119.0\n",
            "records no claim",
        ),
        (
            pinned.clone(),
            Some(
                Record {
                    interface: INTERFACE + 1,
                    ..good.clone()
                }
                .to_toml(),
            ),
            LEDGER,
            "speaks interface",
        ),
    ] {
        let said = check(&script, record.as_deref(), ledger).expect_err(why);
        assert!(said.contains(why), "{why}: {said}");
    }
}

fn windows_record() -> WindowsRecord {
    WindowsRecord(Record {
        verifiers: vec![Receipt {
            arch: "x86_64".into(),
            asset: "aterm-bootstrap-verify-0.119.0-windows-x86_64.exe".into(),
            sha256: "c".repeat(64),
            size: 1_500_000,
        }],
        interface: WINDOWS_INTERFACE,
        ..record()
    })
}

#[test]
fn a_windows_pin_reads_back_as_it_was_written() {
    let pin = windows_record().pin();
    assert_eq!(pin.x86_64, "c".repeat(64));
    let script = format!(
        "# get-aterm.ps1\n$AtermRepo = \"x/y\"\n{}more\n",
        pin.lines()
    );
    assert_eq!(WindowsPin::read(&script), Ok(pin.clone()));
    let empty = format!(
        "# get-aterm.ps1\n$AtermRepo = \"x/y\"\n{}more\n",
        WindowsPin::default().lines()
    );
    assert_eq!(WindowsPin::read(&empty), Ok(WindowsPin::default()));
    assert_eq!(pin.rewrite(&empty), Ok(script.clone()));
    assert_eq!(WindowsPin::default().rewrite(&script), Ok(empty));
    assert_eq!(
        windows_asset_name("0.119.0", "x86_64"),
        "aterm-bootstrap-verify-0.119.0-windows-x86_64.exe"
    );
    // The two scripts' pins are apart: install.sh's lines are no PowerShell pin, nor the reverse.
    assert!(WindowsPin::read(&record().pin().lines()).is_err());
    assert!(Pin::read(&pin.lines()).is_err());
}

#[test]
fn a_windows_pin_of_another_shape_is_refused() {
    let good = windows_record().pin();
    for bad in [
        WindowsPin {
            tag: "0.119.0".into(),
            ..good.clone()
        },
        WindowsPin {
            x86_64: "C".repeat(64),
            ..good.clone()
        },
        WindowsPin {
            x86_64: String::new(),
            ..good.clone()
        },
        WindowsPin {
            repo: "../aterm".into(),
            ..good.clone()
        },
    ] {
        assert!(bad.validate().is_err(), "{bad:?}");
        assert!(WindowsPin::read(&bad.lines()).is_err(), "{bad:?}");
    }
    let twice = format!("{}{}", good.lines(), good.lines());
    assert!(WindowsPin::read(&twice).is_err(), "a key set twice");
    for expands in ["$(Get-Date)", "`$x", r"a\b"] {
        let line = format!("$AtermVerifierTag = \"{expands}\"\n");
        assert!(WindowsPin::read(&line).is_err(), "{expands}");
    }
    let unspaced = good.lines().replace(" = ", "=");
    assert!(
        WindowsPin::read(&unspaced).is_err(),
        "not the script's spelling"
    );
    let apart = good.lines().replacen('\n', "\n# between\n", 1);
    assert!(WindowsPin::read(&apart).is_ok());
    assert!(good.rewrite(&apart).is_err(), "the three lines apart");
}

#[test]
fn a_windows_record_reads_back_and_another_shape_is_refused() {
    let good = windows_record();
    let text = good.to_toml();
    assert!(
        text.starts_with("# get-aterm.ps1's bootstrap verifier pin"),
        "{text}"
    );
    assert_eq!(WindowsRecord::parse(&text), Ok(good.clone()));
    // A Linux record is no Windows record, nor the reverse.
    assert!(WindowsRecord::parse(&record().to_toml()).is_err());
    assert!(Record::parse(&text).is_err());
    let receipt = |f: &dyn Fn(&mut Receipt)| {
        let mut bad = good.clone();
        f(&mut bad.0.verifiers[0]);
        bad
    };
    for bad in [
        receipt(&|r| r.asset = "aterm-bootstrap-verify-0.119.0-linux-x86_64".into()),
        receipt(&|r| r.asset = "aterm-bootstrap-verify-0.118.0-windows-x86_64.exe".into()),
        receipt(&|r| r.arch = "aarch64".into()),
        receipt(&|r| r.sha256 = "C".repeat(64)),
        receipt(&|r| r.size = MAX_ASSET + 1),
        WindowsRecord(Record {
            build_number: 0,
            ..good.0.clone()
        }),
    ] {
        assert!(bad.validate().is_err(), "{bad:?}");
        assert!(WindowsRecord::parse(&bad.to_toml()).is_err(), "{bad:?}");
    }
}

#[test]
fn a_windows_pin_is_held_to_its_record_and_the_ledger() {
    let good = windows_record();
    let pinned = format!("# get-aterm.ps1\n{}", good.pin().lines());
    let text = good.to_toml();
    assert_eq!(check_windows(&pinned, Some(&text), LEDGER), Ok(good.pin()));
    let empty = format!("# get-aterm.ps1\n{}", WindowsPin::default().lines());
    assert_eq!(
        check_windows(&empty, None, LEDGER),
        Ok(WindowsPin::default())
    );
    let mistyped = WindowsPin {
        x86_64: format!("{}d", &good.pin().x86_64[..63]),
        ..good.pin()
    };
    for (script, record, ledger, why) in [
        (pinned.clone(), None, LEDGER, "is missing"),
        (empty, Some(text.clone()), LEDGER, "pins nothing"),
        (
            format!("# get-aterm.ps1\n{}", mistyped.lines()),
            Some(text.clone()),
            LEDGER,
            "is not the one",
        ),
        (
            pinned.clone(),
            Some(text.clone()),
            "1792728567 0.118.0\n",
            "records no claim",
        ),
        (
            pinned,
            Some(
                WindowsRecord(Record {
                    interface: WINDOWS_INTERFACE + 1,
                    ..good.0.clone()
                })
                .to_toml(),
            ),
            LEDGER,
            "speaks interface",
        ),
    ] {
        let said = check_windows(&script, record.as_deref(), ledger).expect_err(why);
        assert!(said.contains(why), "{why}: {said}");
    }
}

/// The appcast fields of [`windows_record`]'s verifier, as a cut signs them.
fn signed_fields(receipt: &Receipt) -> [SignedFields<'_>; 1] {
    [(
        Some(receipt.asset.as_str()),
        Some(receipt.sha256.as_str()),
        Some(receipt.size),
    )]
}

/// What a signed appcast names is the record's receipt, the interface beside it, and nothing
/// when it names nothing (a release cut before the fields, or without a Windows verifier).
#[test]
fn a_signed_windows_verifier_reads_as_the_records_receipt() {
    let good = windows_record();
    let receipt = &good.0.verifiers[0];
    assert_eq!(
        signed_windows_receipts("0.119.0", signed_fields(receipt), Some(WINDOWS_INTERFACE)),
        Ok(Some((good.0.verifiers.clone(), WINDOWS_INTERFACE)))
    );
    assert_eq!(
        signed_windows_receipts("0.119.0", [(None, None, None)], None),
        Ok(None)
    );
    let with = |f: &dyn Fn(&mut Receipt)| {
        let mut bent = receipt.clone();
        f(&mut bent);
        bent
    };
    for (bad, why) in [
        (
            with(&|r| r.asset = "aterm-bootstrap-verify-0.119.0-linux-x86_64".into()),
            "not v0.119.0's canonical asset",
        ),
        (
            with(&|r| r.asset = "aterm-bootstrap-verify-0.118.0-windows-x86_64.exe".into()),
            "not v0.119.0's canonical asset",
        ),
        (
            with(&|r| r.sha256 = "C".repeat(64)),
            "is not 64 lowercase hex digits",
        ),
        (with(&|r| r.size = 63), "outside 64..="),
        (with(&|r| r.size = MAX_ASSET + 1), "outside 64..="),
    ] {
        let said = signed_windows_receipts("0.119.0", signed_fields(&bad), Some(1)).expect_err(why);
        assert!(said.contains(why), "{why}: {said}");
    }
    for (fields, interface, why) in [
        (
            [(Some(receipt.asset.as_str()), None, Some(receipt.size))],
            Some(1),
            "is incomplete",
        ),
        ([(None, None, Some(receipt.size))], Some(1), "is incomplete"),
        (signed_fields(receipt), None, "but no interface"),
        ([(None, None, None)], Some(1), "but no verifier"),
    ] {
        let said = signed_windows_receipts("0.119.0", fields, interface).expect_err(why);
        assert!(said.contains(why), "{why}: {said}");
    }
}

/// A PE32+ image header: `machine`, `characteristics`, optional header `magic`, `subsystem`.
fn pe(machine: u16, characteristics: u16, magic: u16, subsystem: u16) -> Vec<u8> {
    let at = 0x80;
    let mut bytes = vec![0u8; at + 24 + 240];
    bytes[..2].copy_from_slice(b"MZ");
    bytes[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
    bytes[at..at + 4].copy_from_slice(b"PE\0\0");
    bytes[at + 4..at + 6].copy_from_slice(&machine.to_le_bytes());
    bytes[at + 20..at + 22].copy_from_slice(&240u16.to_le_bytes());
    bytes[at + 22..at + 24].copy_from_slice(&characteristics.to_le_bytes());
    bytes[at + 24..at + 26].copy_from_slice(&magic.to_le_bytes());
    bytes[at + 92..at + 94].copy_from_slice(&subsystem.to_le_bytes());
    bytes
}

/// get-aterm.ps1 runs the verifier and reads its standard output: an x86-64 console
/// executable passes, and every other image is refused.
#[test]
fn only_an_x86_64_console_executable_is_a_windows_verifier() {
    assert_eq!(
        windows_verifier_image(&pe(0x8664, 0x0022, 0x20b, 3)),
        Ok(())
    );
    // This test is itself such a program where it runs on Windows x86-64.
    let me = std::fs::read(std::env::current_exe().expect("this test")).expect("its image");
    assert_eq!(
        windows_verifier_image(&me).is_ok(),
        cfg!(all(windows, target_arch = "x86_64")),
        "this test's own image"
    );
    let mut far = pe(0x8664, 0x0022, 0x20b, 3);
    far[0x3c..0x40].copy_from_slice(&0xffff_fff0u32.to_le_bytes());
    for (bad, why) in [
        (b"\x7fELF\x02\x01\x01".to_vec(), "no MZ header"),
        (b"MZ".to_vec(), "names no PE header"),
        (far, "names no PE header"),
        (
            pe(0x8664, 0x0022, 0x20b, 3)[..0x80 + 30].to_vec(),
            "ends before",
        ),
        (pe(0xaa64, 0x0022, 0x20b, 3), "not an x86-64 image"),
        (pe(0x14c, 0x0022, 0x20b, 3), "not an x86-64 image"),
        (pe(0x8664, 0x2022, 0x20b, 3), "not an executable image"),
        (pe(0x8664, 0x0020, 0x20b, 3), "not an executable image"),
        (pe(0x8664, 0x0022, 0x10b, 3), "not a PE32+ image"),
        (pe(0x8664, 0x0022, 0x20b, 2), "not a console program"),
    ] {
        let said = windows_verifier_image(&bad).expect_err(why);
        assert!(said.contains(why), "{why}: {said}");
    }
}

/// The release tooling's pair: get-aterm.ps1 pinned and the record, each what `check_windows`
/// holds to the other; a record the ledger does not claim, of another interface, or a script
/// with no pin to place writes neither.
#[test]
fn the_windows_pin_and_its_record_are_written_together() {
    let good = windows_record();
    let empty = format!("# get-aterm.ps1\n{}rest\n", WindowsPin::default().lines());
    let (script, body) = windows_pin_files(&good, &empty, LEDGER).expect("the pair");
    assert_eq!(
        script,
        format!("# get-aterm.ps1\n{}rest\n", good.pin().lines())
    );
    assert_eq!(body, good.to_toml());
    assert_eq!(check_windows(&script, Some(&body), LEDGER), Ok(good.pin()));
    let other = WindowsRecord(Record {
        interface: WINDOWS_INTERFACE + 1,
        ..good.0.clone()
    });
    for (record, text, ledger, why) in [
        (
            &good,
            empty.as_str(),
            "1792728567 0.118.0\n",
            "records no claim",
        ),
        (&other, empty.as_str(), LEDGER, "speaks interface"),
        (&good, "# get-aterm.ps1\n", LEDGER, "on exactly one line"),
    ] {
        let said = windows_pin_files(record, text, ledger).expect_err(why);
        assert!(said.contains(why), "{why}: {said}");
    }
}
