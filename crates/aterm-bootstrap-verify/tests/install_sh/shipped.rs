// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The shipped build.

use std::path::PathBuf;

use crate::chain::{self, Case, Keys, Laid, Rule, Verifier as _};
use crate::support::Scratch;
use crate::verifier::Built;

const SHIPPED: &str = "ATERM_TEST_BOOTSTRAP_VERIFIER_SHIPPED";

fn shipped() -> PathBuf {
    std::env::var_os(SHIPPED)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            panic!(
                "{SHIPPED} names the static release build of aterm-bootstrap-verify; \
             `targo --unverified verify linux` builds it and runs this"
            )
        })
}

fn is_static_elf(elf: &[u8]) -> bool {
    let at = |offset: usize, len: usize| elf.get(offset..offset + len);
    let n = |offset: usize, len: usize| {
        at(offset, len).map(|bytes| {
            bytes
                .iter()
                .rev()
                .fold(0u64, |acc, byte| (acc << 8) | u64::from(*byte))
        })
    };
    if at(0, 6) != Some(b"\x7fELF\x02\x01".as_slice()) {
        return false;
    }
    let (Some(phoff), Some(phentsize), Some(phnum)) = (n(32, 8), n(54, 2), n(56, 2)) else {
        return false;
    };
    (0..phnum).all(|i| {
        let entry = usize::try_from(phoff + i * phentsize).unwrap_or(usize::MAX);
        n(entry, 4).is_some_and(|kind| kind != 3)
    })
}

fn paper_master_alone(built: &Built) -> Result<usize, String> {
    let scratch = Scratch::new();
    let keys = Keys::new();
    let cases = chain::corpus(&keys);
    let laid: Vec<Laid> = cases
        .iter()
        .enumerate()
        .map(|(n, case)| case.lay(&keys, &scratch.mkdir(&format!("corpus/{n}"))))
        .collect();
    let runs: Vec<(&Case, &Laid)> = cases.iter().zip(&laid).collect();
    let answers = built.mains(&scratch, &runs);
    assert_eq!(answers.len(), cases.len());
    let mut admitted_elsewhere = 0;
    for ((case, laid), answer) in runs.iter().zip(&answers) {
        if !answer.starts_with("refused\t") {
            return Err(format!("{}: {answer}", case.label));
        }
        if case.rule == Rule::Shared && chain::updater_verdict(case, &laid.proofs).is_ok() {
            admitted_elsewhere += 1;
            if answer != "refused\troster is not signed by the pinned paper master" {
                return Err(format!("{}: {answer}", case.label));
            }
        }
    }
    Ok(admitted_elsewhere)
}

#[test]
#[ignore = "the Linux guest lane runs it with ATERM_TEST_BOOTSTRAP_VERIFIER_SHIPPED"]
fn the_shipped_verifier_is_static_and_trusts_the_paper_master_alone() {
    let exe = shipped();
    let bytes = std::fs::read(&exe).unwrap_or_else(|e| panic!("{}: {e}", exe.display()));
    assert!(
        is_static_elf(&bytes),
        "{} is not a static ELF",
        exe.display()
    );
    let admitted_elsewhere =
        paper_master_alone(&Built::at(exe)).unwrap_or_else(|why| panic!("{why}"));
    assert!(
        admitted_elsewhere >= 50,
        "{admitted_elsewhere} cases the test anchor admits"
    );
}

#[test]
fn the_paper_master_check_catches_a_build_that_reads_the_test_anchor() {
    let why = paper_master_alone(&Built::new()).expect_err("the suite's build reads the anchor");
    assert!(why.starts_with("a valid release: ok\t"), "{why}");
}

#[test]
fn the_static_elf_reader_finds_an_interpreter() {
    let mut header = chain::elf("x86_64");
    assert!(is_static_elf(&header), "no program headers");
    header[32..40].copy_from_slice(&64u64.to_le_bytes());
    header[54..56].copy_from_slice(&56u16.to_le_bytes());
    header[56..58].copy_from_slice(&1u16.to_le_bytes());
    let mut load = vec![0u8; 56];
    load[..4].copy_from_slice(&1u32.to_le_bytes());
    let mut dynamic = header[..64].to_vec();
    dynamic.extend_from_slice(&load);
    assert!(is_static_elf(&dynamic), "a PT_LOAD alone");
    assert!(
        !is_static_elf(&dynamic[..66]),
        "a truncated program header table"
    );
    dynamic[64..68].copy_from_slice(&3u32.to_le_bytes());
    assert!(!is_static_elf(&dynamic), "a PT_INTERP");
    assert!(!is_static_elf(b"#!/bin/sh\n"), "not an ELF");
}
