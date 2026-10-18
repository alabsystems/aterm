// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The producer's half of LINEAGE over synthetic trees: the payload scan reads compiled
//! output only, and a stamped `LINEAGE` is admitted by the client's own check against the
//! shipped bytes — and refused the moment the record or a byte it names is edited.

use std::path::{Path, PathBuf};

use atpkg::lineage::{
    LINEAGE_FILE, LineagePolicy, LineageProof, SEED_MINT_FILE, SEED_MINT_SCHEMA,
    check_compiler_with,
};
use atpkg_publish::artifact::lineage;

/// A Trust compiler's commit (the 7d5533b2 seal's).
const TRUST: &str = "7d5533b2bb9c9417bc09c27f1acd9d0b808f8f15";
/// Stock 1.97.1's commit (store ay 8256 embeds it 451 times).
const STOCK: &str = "8bab26f4f68e0e26f0bb7960be334d5b520ea452";
/// A Mach-O thin 64-bit head: what the client's `native_head` admits.
const MACHO: &[u8] = &[0xcf, 0xfa, 0xed, 0xfe, 0x0c, 0x00, 0x00, 0x01];

/// A scratch dir (removed on drop however the test ends), and its path.
fn tmp(label: &str) -> (aterm_tempfile::TempDir, PathBuf) {
    let dir = aterm_tempfile::Builder::new()
        .prefix(&format!("atpkg-publish-lineage-{label}-"))
        .tempdir()
        .expect("scratch dir");
    let path = dir.path().to_path_buf();
    (dir, path)
}

fn object(bytes: &[u8]) -> Vec<u8> {
    let mut v = MACHO.to_vec();
    v.extend_from_slice(b"\0\0padding\0");
    v.extend_from_slice(bytes);
    v.extend_from_slice(b"\0trailer");
    v
}

/// A crate-metadata header, as rustc writes it (`rustc_serialize`'s `emit_str`): the
/// length as unsigned LEB128, the string, then the `0xc1` string sentinel.
fn metadata_header(version: &str) -> Vec<u8> {
    let mut v = b"rust\0\0\0\n\0\0\0\0\0\0\0\0".to_vec();
    let mut len = version.len();
    loop {
        let low = u8::try_from(len & 0x7f).expect("seven bits");
        len >>= 7;
        if len == 0 {
            v.push(low);
            break;
        }
        v.push(low | 0x80);
    }
    v.extend_from_slice(version.as_bytes());
    v.extend_from_slice(b"\xc1\x02\x99");
    v
}

fn trust_vv(commit: &str) -> String {
    format!(
        "rustc 1.99.0-dev ({} 2026-09-23) (trustc 0.1.0)\nbinary: trustc\ncommit-hash: {commit}\n\
         commit-date: 2026-09-23\nhost: aarch64-apple-darwin\nrelease: 1.99.0-dev\ntrust: 0.1.0\n\
         LLVM version: 22.1.2\n",
        &commit[..9]
    )
}

/// A toolchain root: bin/{trustc,targo} and lib/librustc_driver-<h>.dylib, each a
/// Mach-O-headed file carrying `extra`.
fn toolchain(root: &Path, extra: &[u8]) -> PathBuf {
    std::fs::create_dir_all(root.join("bin")).expect("mkdir");
    std::fs::create_dir_all(root.join("lib")).expect("mkdir");
    std::fs::write(
        root.join("bin/trustc"),
        object(&[b"trustc ", extra].concat()),
    )
    .expect("write");
    std::fs::write(root.join("bin/targo"), object(&[b"targo ", extra].concat())).expect("write");
    std::fs::write(
        root.join("lib/librustc_driver-0123456789abcdef.dylib"),
        object(&[b"driver ", extra].concat()),
    )
    .expect("write");
    root.join("bin")
}

fn probe_says(text: String) -> impl Fn(&Path) -> Result<String, String> {
    move |_| Ok(text.clone())
}

fn sha(path: &Path) -> String {
    atpkg::tree::sha256_file(path).expect("hash")
}

/// A SEED-MINT for the toolchain at `root` (commit `commit`), comparing trustc and the
/// driver, with `kv` fields replaced.
fn seed_mint(root: &Path, commit: &str, kv: &[(&str, &str)]) {
    let tree = "a".repeat(64);
    let mut fields: Vec<(String, String)> = [
        ("schema", SEED_MINT_SCHEMA),
        ("source_commit", commit),
        (
            "pinned_seed_commit",
            "1111111111111111111111111111111111111111",
        ),
        ("ddc_seed_relation", "pin-ancestor"),
        (
            "ddc_seed_commit",
            "2222222222222222222222222222222222222222",
        ),
        ("stage2_tree_sha256_pinned_seed", tree.as_str()),
        ("stage2_tree_sha256_ddc_seed", tree.as_str()),
        ("ddc_verdict", "identical"),
        ("repin_commit", "3333333333333333333333333333333333333333"),
        ("repin_compiler_git_commit_hash", commit),
    ]
    .iter()
    .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
    .collect();
    for (k, v) in kv {
        if let Some(slot) = fields.iter_mut().find(|(key, _)| key == k) {
            slot.1 = (*v).to_string();
        }
    }
    let mut text = String::from("# SEED-MINT (fixture)\n");
    for (k, v) in &fields {
        text.push_str(&format!("{k}={v}\n"));
    }
    text.push_str(&format!(
        "compiler_file={} bin/trustc\n",
        sha(&root.join("bin/trustc"))
    ));
    text.push_str(&format!(
        "compiler_file={} lib/librustc_driver-0123456789abcdef.dylib\n",
        sha(&root.join("lib/librustc_driver-0123456789abcdef.dylib"))
    ));
    std::fs::write(root.join(SEED_MINT_FILE), text).expect("write SEED-MINT");
}

#[test]
fn scan_tree_reads_compiled_output_only() {
    let (_dir, d) = tmp("tree");
    std::fs::create_dir_all(d.join("bin")).expect("mkdir");
    std::fs::create_dir_all(d.join("lib/rustlib/t/lib")).expect("mkdir");
    let stock = format!("/rustc/{STOCK}/library/std/src/x.rs");
    std::fs::write(d.join("bin/ay"), object(stock.as_bytes())).expect("write");
    std::fs::write(d.join("bin/clean"), object(b"nothing foreign")).expect("write");
    // A text file that QUOTES a stock path is documentation, not compiled output.
    std::fs::write(d.join("README"), stock.as_bytes()).expect("write");
    std::fs::write(
        d.join("lib/rustlib/t/lib/libstd-1.rlib"),
        [
            b"!<arch>\n".as_slice(),
            &metadata_header("rustc 1.97.1 (8bab26f4f 2026-07-14)"),
        ]
        .concat(),
    )
    .expect("write");
    let report = lineage::scan_tree(&d, TRUST).expect("scan");
    assert_eq!(report.objects, 3);
    let files: Vec<&str> = report.findings.iter().map(|f| f.file.as_str()).collect();
    assert_eq!(files, vec!["bin/ay", "lib/rustlib/t/lib/libstd-1.rlib"]);
}

/// The bundle's round trip: a seal admitted by SEED-MINT is staged, its bytes change
/// (strip, relocate, re-sign), LINEAGE is stamped over the SHIPPED bytes, and the
/// staged tree is then admitted by LINEAGE — and refused the moment the record or a
/// byte it names is edited.
#[test]
fn a_stamped_lineage_admits_the_shipped_bytes_and_refuses_an_edit() {
    let (_dir, d) = tmp("stamp");
    let seal = d.join("seal");
    let seal_bin = toolchain(&seal, b"");
    seed_mint(&seal, TRUST, &[]);
    let stage = d.join("stage");
    toolchain(&stage, b"stripped");
    std::fs::copy(seal.join(SEED_MINT_FILE), stage.join(SEED_MINT_FILE)).expect("copy receipt");
    let probe = probe_says(trust_vv(TRUST));
    // Before the stamp, the stripped bytes are not what SEED-MINT compared.
    assert!(check_compiler_with(&stage.join("bin"), LineagePolicy::Require, &probe).is_err());
    // The stamp vouches only for the source's own compiler and receipt: a staged trustc of
    // another commit, or a staged SEED-MINT other than the source's, stamps nothing.
    let other = "4444444444444444444444444444444444444444";
    let staged = |p: &Path| p.components().any(|c| c.as_os_str() == "stage");
    let stage_says_other = |p: &Path| Ok(trust_vv(if staged(p) { other } else { TRUST }));
    let err =
        lineage::stamp_with(&stage, &seal_bin, &stage_says_other).expect_err("another commit");
    assert!(
        err.iter()
            .any(|l| l.contains(&format!("the staged bin/trustc reports {other}"))),
        "{err:?}"
    );
    seed_mint(&stage, TRUST, &[("pinned_seed_commit", other)]);
    let err = lineage::stamp_with(&stage, &seal_bin, &probe).expect_err("another staged receipt");
    assert!(
        err.iter().any(|l| l.contains("the staged SEED-MINT is")),
        "{err:?}"
    );
    assert!(!stage.join(LINEAGE_FILE).exists());
    std::fs::copy(seal.join(SEED_MINT_FILE), stage.join(SEED_MINT_FILE)).expect("restore");
    let text = lineage::stamp_with(&stage, &seal_bin, &probe).expect("stamp");
    assert!(
        text.contains(&format!("trustc_sha256={}", sha(&stage.join("bin/trustc")))),
        "{text}"
    );
    let admitted =
        check_compiler_with(&stage.join("bin"), LineagePolicy::Require, &probe).expect("admitted");
    assert!(matches!(admitted.lineage, LineageProof::Lineage { .. }));

    // Tamper with the record.
    let rec = std::fs::read_to_string(stage.join(LINEAGE_FILE)).expect("read");
    std::fs::write(
        stage.join(LINEAGE_FILE),
        rec.replace(
            "ddc_seed_relation=pin-ancestor",
            "ddc_seed_relation=lineage-disjoint",
        ),
    )
    .expect("write");
    let err = check_compiler_with(&stage.join("bin"), LineagePolicy::Dormant, &probe)
        .expect_err("edited record");
    assert!(
        err.iter().any(|l| l.contains("ddc_seed_relation")),
        "{err:?}"
    );
    std::fs::write(stage.join(LINEAGE_FILE), &rec).expect("restore");
    // Tamper with a byte it names.
    std::fs::write(stage.join("bin/targo"), object(b"targo swapped")).expect("write");
    let err = check_compiler_with(&stage.join("bin"), LineagePolicy::Require, &probe)
        .expect_err("edited bytes");
    assert!(err.iter().any(|l| l.contains("bin/targo")), "{err:?}");
    std::fs::write(stage.join("bin/targo"), object(b"targo stripped")).expect("restore");
    check_compiler_with(&stage.join("bin"), LineagePolicy::Require, &probe)
        .expect("the restored tree is admitted again");
    // Swap the driver — what trust-mc 20065 shipped from rustup's nightly (TP-7). Nothing
    // but the record's digest names the driver a stamped tree runs.
    let driver = stage.join("lib/librustc_driver-0123456789abcdef.dylib");
    let shipped_driver = std::fs::read(&driver).expect("read driver");
    std::fs::write(&driver, object(b"driver swapped")).expect("write");
    let err = check_compiler_with(&stage.join("bin"), LineagePolicy::Require, &probe)
        .expect_err("swapped driver");
    assert!(
        err.iter().any(|l| l.contains("records the driver as")),
        "{err:?}"
    );
    std::fs::write(&driver, shipped_driver).expect("restore");
    // Replace the tree's SEED-MINT with another receipt of the SAME commit, one that passes
    // every check of its own: the record is bound to the receipt it was stamped against.
    let stamped_mint = std::fs::read(stage.join(SEED_MINT_FILE)).expect("read receipt");
    seed_mint(
        &stage,
        TRUST,
        &[(
            "pinned_seed_commit",
            "4444444444444444444444444444444444444444",
        )],
    );
    let err = check_compiler_with(&stage.join("bin"), LineagePolicy::Dormant, &probe)
        .expect_err("another receipt");
    assert!(
        err.iter()
            .any(|l| l.contains("but the tree's SEED-MINT is")),
        "{err:?}"
    );
    std::fs::write(stage.join(SEED_MINT_FILE), &stamped_mint).expect("restore");
    // …and the record's own digest of it, edited.
    let receipt = format!(
        "lineage_receipt_sha256={}",
        sha(&stage.join(SEED_MINT_FILE))
    );
    assert!(rec.contains(&receipt), "{rec}");
    std::fs::write(
        stage.join(LINEAGE_FILE),
        rec.replace(
            &receipt,
            &format!("lineage_receipt_sha256={}", "0".repeat(64)),
        ),
    )
    .expect("write");
    let err = check_compiler_with(&stage.join("bin"), LineagePolicy::Dormant, &probe)
        .expect_err("edited receipt digest");
    assert!(
        err.iter()
            .any(|l| l.contains(&format!("names SEED-MINT {}", "0".repeat(64)))),
        "{err:?}"
    );
    std::fs::write(stage.join(LINEAGE_FILE), &rec).expect("restore");
    check_compiler_with(&stage.join("bin"), LineagePolicy::Require, &probe)
        .expect("the restored tree is admitted again");

    // A source whose lineage is unproven stamps nothing.
    std::fs::remove_file(seal.join(SEED_MINT_FILE)).expect("rm");
    let _ = std::fs::remove_file(stage.join(LINEAGE_FILE));
    assert!(lineage::stamp_with(&stage, &seal_bin, &probe).is_err());
    assert!(!stage.join(LINEAGE_FILE).exists());
}
