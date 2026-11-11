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
