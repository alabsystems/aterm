// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The arch gate: the reader over crafted headers, the claim checked against the bytes for
//! every client target and every mismatch, and the tree gate the bundle lane runs over its
//! `bin/`.

use std::path::{Path, PathBuf};

use atpkg_publish::artifact::arch::{self, Cpu, Format, Object, Refusal};
use atpkg_publish::triple::Triple;

use super::fixture::{
    CPU_ARM64, CPU_X86_64, EM_AARCH64, EM_X86_64, PE_AMD64, PE_ARM64, elf, fat, macho, pe, put,
    put_exe, scratch,
};

fn t(s: &str) -> Triple {
    Triple::parse(s).expect("a client target")
}

fn obj(format: Format, cpus: Vec<Cpu>) -> Object {
    Object { format, cpus }
}

/// One correctly built binary per client target, by name.
fn per_target(dir: &Path) -> Vec<(Triple, PathBuf)> {
    [
        ("aarch64-apple-darwin", macho(CPU_ARM64)),
        ("x86_64-apple-darwin", macho(CPU_X86_64)),
        ("x86_64-unknown-linux-gnu", elf(EM_X86_64, 2, 1)),
        ("aarch64-unknown-linux-gnu", elf(EM_AARCH64, 2, 1)),
        ("x86_64-pc-windows-msvc", pe(PE_AMD64)),
        ("aarch64-pc-windows-msvc", pe(PE_ARM64)),
    ]
    .into_iter()
    .map(|(name, bytes)| (t(name), put_exe(&dir.join(name), &bytes)))
    .collect()
}

#[test]
fn every_client_target_expects_its_own_format_and_architecture() {
    let want = [
        ("aarch64-apple-darwin", Format::MachO, Cpu::Arm64),
        ("x86_64-apple-darwin", Format::MachO, Cpu::X86_64),
        ("x86_64-unknown-linux-gnu", Format::Elf, Cpu::X86_64),
        ("aarch64-unknown-linux-gnu", Format::Elf, Cpu::Arm64),
        ("x86_64-pc-windows-msvc", Format::Pe, Cpu::X86_64),
        ("aarch64-pc-windows-msvc", Format::Pe, Cpu::Arm64),
    ];
    assert_eq!(
        Triple::all().count(),
        want.len(),
        "a target the client gained needs a row here"
    );
    for (name, format, cpu) in want {
        assert_eq!(arch::expect(t(name)), (format, cpu), "{name}");
    }
}

#[test]
fn the_reader_names_the_format_and_every_architecture_out_of_the_header() {
    let tmp = scratch("arch-read");
    let d = tmp.path();
    let cases: [(&str, Vec<u8>, Object); 11] = [
        (
            "elf-x86",
            elf(EM_X86_64, 2, 1),
            obj(Format::Elf, vec![Cpu::X86_64]),
        ),
        (
            "elf-arm",
            elf(EM_AARCH64, 2, 1),
            obj(Format::Elf, vec![Cpu::Arm64]),
        ),
        (
            "elf-riscv",
            elf(243, 2, 1),
            obj(Format::Elf, vec![Cpu::Other("e_machine=243".into())]),
        ),
        (
            "elf32",
            elf(3, 1, 1),
            obj(Format::Elf, vec![Cpu::Other("class=1 data=1".into())]),
        ),
        (
            "elf-be",
            elf(EM_AARCH64, 2, 2),
            obj(Format::Elf, vec![Cpu::Other("class=2 data=2".into())]),
        ),
        (
            "macho-x86",
            macho(CPU_X86_64),
            obj(Format::MachO, vec![Cpu::X86_64]),
        ),
        (
            "macho-arm",
            macho(CPU_ARM64),
            obj(Format::MachO, vec![Cpu::Arm64]),
        ),
        (
            "fat",
            fat(&[CPU_X86_64, CPU_ARM64]),
            obj(Format::MachO, vec![Cpu::X86_64, Cpu::Arm64]),
        ),
        ("pe-x86", pe(PE_AMD64), obj(Format::Pe, vec![Cpu::X86_64])),
        ("pe-arm", pe(PE_ARM64), obj(Format::Pe, vec![Cpu::Arm64])),
        (
            "pe-i386",
            pe(0x14c),
            obj(Format::Pe, vec![Cpu::Other("machine=332".into())]),
        ),
    ];
    for (name, bytes, want) in cases {
        let p = put_exe(&d.join(name), &bytes);
        assert_eq!(arch::read(&p), Some(want), "{name}");
    }
    assert_eq!(
        arch::read(&put_exe(&d.join("fat-i386"), &fat(&[7]))).map(|o| o.to_string()),
        Some("macho i386".to_string())
    );
}

#[test]
fn what_is_not_an_object_is_unreadable_never_an_architecture() {
    let tmp = scratch("arch-unreadable");
    let d = tmp.path();
    let mut java = vec![0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 52];
    java.resize(64, 0);
    for (name, bytes) in [
        (
            "script.sh",
            b"#!/bin/sh\necho hi, a script long enough\n".to_vec(),
        ),
        ("plain.txt", b"aterm aterm aterm aterm aterm\n".to_vec()),
        ("lib.a", b"!<arch>\nfixture archive member body\n".to_vec()),
        ("empty", Vec::new()),
        ("short-elf", b"\x7fELF\x02\x01".to_vec()),
        ("java.class", java),
    ] {
        put(&d.join(name), &bytes);
        assert_eq!(arch::read(&d.join(name)), None, "{name}");
    }
    assert_eq!(arch::read(&d.join("no-such-file")), None);
}

#[test]
fn the_reader_agrees_with_the_host_about_a_real_binary() {
    let Some(host) = Triple::host() else {
        return;
    };
    let (format, cpu) = arch::expect(host);
    let mut read = 0;
    for real in ["/bin/sh", "/bin/ls"] {
        let p = Path::new(real);
        if !p.is_file() {
            continue;
        }
        let got = arch::read(p).unwrap_or_else(|| panic!("{real} is a real binary on this box"));
        assert_eq!(got.format, format, "{real}: {got}");
        assert!(got.cpus.contains(&cpu), "{real} reads as [{got}] on {host}");
        read += 1;
    }
    assert!(read > 0, "a unix host carries /bin/sh");
}

/// Every client target's claim against every target's binary: the matching binary passes;
/// one of another OS is a format mismatch (an arch word alone cannot see that an ELF does
/// not exec on macOS); one of the same OS and another architecture is an architecture
/// mismatch.
#[test]
fn every_claim_against_every_targets_binary_passes_only_its_own() {
    let tmp = scratch("arch-matrix");
    let bins = per_target(tmp.path());
    for (claim, _) in &bins {
        for (built, file) in &bins {
            let got = arch::gate(*claim, std::slice::from_ref(file));
            if claim == built {
                assert_eq!(got, Ok(1), "{claim} over its own binary");
                continue;
            }
            let read = arch::read(file).expect("an object");
            let want = if claim.os() == built.os() {
                Refusal::ArchMismatch {
                    file: file.clone(),
                    got: read,
                    triple: *claim,
                }
            } else {
                Refusal::FormatMismatch {
                    file: file.clone(),
                    got: read,
                    triple: *claim,
                }
            };
            assert_eq!(got, Err(want), "{claim} over a {built} binary");
        }
    }
}

#[test]
fn a_universal_binary_passes_a_claim_it_carries_a_slice_for_and_no_other() {
    let tmp = scratch("arch-fat");
    let both = put_exe(&tmp.path().join("both"), &fat(&[CPU_X86_64, CPU_ARM64]));
    let x86 = put_exe(&tmp.path().join("x86"), &fat(&[CPU_X86_64]));
    assert_eq!(
        arch::gate(t("x86_64-apple-darwin"), std::slice::from_ref(&both)),
        Ok(1)
    );
    assert_eq!(arch::gate(t("aarch64-apple-darwin"), &[both]), Ok(1));
    assert!(matches!(
        arch::gate(t("aarch64-apple-darwin"), &[x86]),
        Err(Refusal::ArchMismatch { .. })
    ));
}

#[test]
fn a_32_bit_elf_a_script_and_an_empty_list_are_refused() {
    let tmp = scratch("arch-refusals");
    let d = tmp.path();
    let linux = t("x86_64-unknown-linux-gnu");
    let elf32 = put_exe(&d.join("elf32"), &elf(3, 1, 1));
    assert!(matches!(
        arch::gate(linux, &[elf32]),
        Err(Refusal::ArchMismatch { .. })
    ));
    let script = put_exe(&d.join("s"), b"#!/bin/sh\necho a script, not an object\n");
    assert_eq!(
        arch::gate(linux, std::slice::from_ref(&script)),
        Err(Refusal::Unreadable {
            file: script,
            triple: linux
        })
    );
    assert_eq!(
        arch::gate(linux, &[]),
        Err(Refusal::NoObjects {
            triple: linux,
            scripts: 0
        }),
        "a gate that reads nothing proves nothing"
    );
}

#[test]
fn every_named_binary_is_read_not_only_the_first() {
    let tmp = scratch("arch-every");
    let d = tmp.path();
    let good = put_exe(&d.join("good"), &elf(EM_X86_64, 2, 1));
    let foreign = put_exe(&d.join("foreign"), &elf(EM_AARCH64, 2, 1));
    let linux = t("x86_64-unknown-linux-gnu");
    let err = arch::gate(linux, &[good.clone(), good.clone(), foreign.clone()])
        .expect_err("a foreign binary in position 3");
    assert!(matches!(&err, Refusal::ArchMismatch { file, .. } if *file == foreign));
    let text = err.to_string();
    assert!(
        text.contains("ARCHITECTURE MISMATCH") && text.contains("[elf arm64]"),
        "the refusal names the gate and what it read: {text}"
    );
    assert_eq!(arch::gate(linux, &[good.clone(), good]), Ok(2));
}

#[test]
fn the_tree_gate_reads_every_object_names_every_script_and_refuses_nothing_to_read() {
    let tmp = scratch("arch-tree");
    let d = tmp.path();
    let linux = t("x86_64-unknown-linux-gnu");
    let x86 = elf(EM_X86_64, 2, 1);
    let script = b"#!/bin/sh\nexec gdb \"$@\"\n";

    put_exe(&d.join("good/trustc"), &x86);
    put_exe(&d.join("good/sub/targo"), &x86);
    put_exe(&d.join("good/rust-gdb"), script);
    assert_eq!(
        arch::gate_tree(linux, &d.join("good")),
        Ok(arch::TreeGate {
            objects: 2,
            scripts: vec!["rust-gdb".into()]
        })
    );

    put_exe(&d.join("mixed/trustc"), &x86);
    put_exe(&d.join("mixed/tippy"), &elf(EM_AARCH64, 2, 1));
    assert!(matches!(
        arch::gate_tree(linux, &d.join("mixed")),
        Err(Refusal::ArchMismatch { .. })
    ));

    put_exe(&d.join("scripts/rust-gdb"), script);
    assert_eq!(
        arch::gate_tree(linux, &d.join("scripts")),
        Err(Refusal::NoObjects {
            triple: linux,
            scripts: 1
        })
    );
    std::fs::create_dir_all(d.join("empty")).expect("mkdir");
    assert_eq!(
        arch::gate_tree(linux, &d.join("empty")),
        Err(Refusal::NoObjects {
            triple: linux,
            scripts: 0
        })
    );
    assert_eq!(
        arch::gate_tree(linux, &d.join("missing")),
        Err(Refusal::MissingDir {
            dir: d.join("missing")
        })
    );
}
