// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `lineage` over synthetic trees: the scan's evidence classes (each with the prose or shape
//! that must not count), compiler admission by content (the `-vV` probe injected, everything
//! else real files), the SEED-MINT messages and the LINEAGE record's round trip.

use super::*;

/// A Trust compiler's commit.
const TRUST: &str = "7d5533b2bb9c9417bc09c27f1acd9d0b808f8f15";
/// Stock rustc 1.97.1's commit.
const STOCK: &str = "8bab26f4f68e0e26f0bb7960be334d5b520ea452";
/// A Mach-O thin 64-bit head: what `native_head` admits.
const MACHO: &[u8] = &[0xcf, 0xfa, 0xed, 0xfe, 0x0c, 0x00, 0x00, 0x01];

/// A scratch dir (removed on drop) and its path.
fn tmp(label: &str) -> (crate::testdir::Dir, PathBuf) {
    let dir = crate::testdir::dir(&format!("atpkg-lineage-{label}-"));
    let path = dir.to_path_buf();
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

fn scan_bytes(bytes: &[u8], admitted: &str) -> Vec<Evidence> {
    let (_dir, d) = tmp("scan-bytes");
    let f = d.join("obj");
    std::fs::write(&f, bytes).expect("write object");
    let (hits, _) = scan_file(&f, admitted, false).expect("scan");
    hits.into_keys().collect()
}

#[test]
fn a_stock_std_path_is_refused_and_named() {
    let path = format!("/rustc/{STOCK}/library/std/src/x.rs");
    let found = scan_bytes(&object(path.as_bytes()), TRUST);
    assert_eq!(found, vec![Evidence::StdPath(STOCK.to_string())]);
    let described = Finding {
        file: "bin/ay".to_string(),
        evidence: found[0].clone(),
        count: 451,
    }
    .describe();
    assert!(described.contains(STOCK), "{described}");
    // The admitted compiler's own commit is not evidence against it.
    assert!(scan_bytes(&object(path.as_bytes()), STOCK).is_empty());
    // Not a 40-hex commit: not a std path.
    assert!(scan_bytes(&object(b"/rustc/not-a-commit/library/std/src/x.rs"), TRUST).is_empty());
}

#[test]
fn a_stock_metadata_header_is_refused_and_trusts_own_is_not() {
    let stock = "rustc 1.97.1 (8bab26f4f 2026-07-14)";
    assert_eq!(
        scan_bytes(&object(&metadata_header(stock)), TRUST),
        vec![Evidence::MetadataVersion(stock.to_string())]
    );
    let nightly = "rustc 1.93.0-nightly (646a3f8c1 2025-12-02)";
    assert_eq!(
        scan_bytes(&object(&metadata_header(nightly)), TRUST),
        vec![Evidence::MetadataVersion(nightly.to_string())]
    );
    // The admitted compiler's own header (Trust's carries no `(trustc …)` suffix there).
    assert!(
        scan_bytes(
            &object(&metadata_header("rustc 1.99.0-dev (7d5533b2b 2026-09-23)")),
            TRUST
        )
        .is_empty()
    );
    // Another Trust commit's metadata is not this compiler's either.
    assert_eq!(
        scan_bytes(
            &object(&metadata_header("rustc 1.99.0-dev (321aaeda7 2026-09-17)")),
            TRUST
        )
        .len(),
        1
    );
}

#[test]
fn a_beta_or_distribution_built_compiler_is_evidence() {
    // Every shape a stock rustc writes into crate metadata: a beta channel's
    // `<num>-beta.<n>`, and the ` (<description>)` a distribution appends.
    for v in [
        "rustc 1.81.0-beta.3 (4c39e7b6b 2024-07-29)",
        "rustc 1.82.0 (f6e511eec 2024-10-15) (Homebrew)",
        "rustc 1.75.0 (82e1608df 2023-12-21) (Fedora 1.75.0-1.fc39)",
        "rustc 1.83.0-beta.6 (90b35a623 2024-11-26) (Arch Linux rust 1:1.83.0-1)",
    ] {
        assert_eq!(
            scan_bytes(&object(&metadata_header(v)), TRUST),
            vec![Evidence::MetadataVersion(v.to_string())],
            "{v}"
        );
    }
    // A description long enough that the length takes two LEB128 bytes.
    let long = format!(
        "rustc 1.82.0 (f6e511eec 2024-10-15) ({})",
        "a distribution's own words ".repeat(5)
    );
    assert!(long.len() > 127);
    assert_eq!(
        scan_bytes(&object(&metadata_header(&long)), TRUST),
        vec![Evidence::MetadataVersion(long.clone())]
    );
    // The same shapes as an LLVM producer / ELF `.comment`.
    for (producer, named) in [
        (
            "clang LLVM (rustc version 1.81.0-beta.3 (4c39e7b6b 2024-07-29))",
            "rustc version 1.81.0-beta.3 (4c39e7b6b 2024-07-29)",
        ),
        (
            "rustc version 1.82.0 (f6e511eec 2024-10-15) (Homebrew)\0",
            "rustc version 1.82.0 (f6e511eec 2024-10-15)",
        ),
    ] {
        assert_eq!(
            scan_bytes(&object(producer.as_bytes()), TRUST),
            vec![Evidence::ProducerVersion(named.to_string())],
            "{producer}"
        );
    }
    // The admitted compiler under a description of its own is still the admitted one.
    assert!(
        scan_bytes(
            &object(&metadata_header(
                "rustc 1.99.0-dev (7d5533b2b 2026-09-23) (trustc 0.1.0)"
            )),
            TRUST
        )
        .is_empty()
    );
    // A length that runs past the version into anything but ` (…)` is no header: a string
    // that merely starts with a version (prose, a self-report) is not one.
    let mut not_a_header = b"rust\0\0\0\n\0\0\0\0\0\0\0\0".to_vec();
    let text = b"rustc 1.97.1 (8bab26f4f 2026-07-14) was the stock compiler we measured";
    not_a_header.push(u8::try_from(text.len()).expect("short"));
    not_a_header.extend_from_slice(text);
    assert!(scan_bytes(&object(&not_a_header), TRUST).is_empty());
    let mut control = b"rust\0\0\0\n\0\0\0\0\0\0\0\0".to_vec();
    let text = b"rustc 1.97.1 (8bab26f4f 2026-07-14) (a\0b)";
    control.push(u8::try_from(text.len()).expect("short"));
    control.extend_from_slice(text);
    assert!(scan_bytes(&object(&control), TRUST).is_empty());
}

#[test]
fn prose_and_self_reports_are_not_evidence() {
    // A doc comment quoting a stock version, as Trust's own crate metadata carries: space-led,
    // so no length byte can match it.
    let prose = b" not work either. MEASURED on stock rustc 1.97.1 (8bab26f4f 2026-07-14),";
    assert!(scan_bytes(&object(prose), TRUST).is_empty());
    // A backtick-quoted `trustc 1.99.0-dev (…)` (libtrust_types).
    assert!(
        scan_bytes(
            &object(b"MEASURED on `trustc 1.99.0-dev (ddb545d35 2026-08-21)`"),
            TRUST
        )
        .is_empty()
    );
    // A program's build-info self-report.
    let self_report = b"another copyrustc 1.99.0-dev (7d5533b2b 2026-09-23) (trustc 0.1.0)";
    assert!(scan_bytes(&object(self_report), TRUST).is_empty());
    assert!(scan_bytes(&object(self_report), STOCK).is_empty());
    // aterm's own help text names rustup's `trust` channel link, not a toolchain path.
    assert!(
        scan_bytes(
            &object(b"~/.rustup/toolchains/trust -> <prefix>/rustup/trust"),
            TRUST
        )
        .is_empty()
    );
}

#[test]
fn producer_wasm_and_rustup_evidence_is_refused() {
    let producer = b"clang LLVM (rustc version 1.97.1 (8bab26f4f 2026-07-14))";
    assert_eq!(
        scan_bytes(&object(producer), TRUST),
        vec![Evidence::ProducerVersion(
            "rustc version 1.97.1 (8bab26f4f 2026-07-14)".to_string()
        )]
    );
    assert!(
        scan_bytes(
            &object(b"clang LLVM (rustc version 1.99.0-dev (7d5533b2b 2026-09-23))"),
            TRUST
        )
        .is_empty()
    );
    // A wasm producers section.
    let mut wasm =
        b"\0asm\x01\0\0\0\0p\tproducers\x02\x08language\x01\x04Rust\0\x0cprocessed-by\x03\x05rustc"
            .to_vec();
    let v = "1.97.0 (2d8144b78 2026-07-07)";
    wasm.push(u8::try_from(v.len()).expect("short"));
    wasm.extend_from_slice(v.as_bytes());
    wasm.extend_from_slice(b"\x06walrus\x060.24.4");
    assert_eq!(
        scan_bytes(&wasm, TRUST),
        vec![Evidence::WasmProducer(v.to_string())]
    );
    let path = b"/Users//x/.rustup/toolchains/nightly-2025-12-03-aarch64-apple-darwin/lib/rustlib/src/rust/library/core/src/lib.rs";
    assert_eq!(
        scan_bytes(&object(path), TRUST),
        vec![Evidence::RustupPath(
            "nightly-2025-12-03-aarch64-apple-darwin".to_string()
        )]
    );
    // A system RUSTUP_HOME (the Linux images' /usr/local/rustup) is the same toolchain tree.
    let system = b"/usr/local/rustup/toolchains/1.97.1-x86_64-unknown-linux-gnu/lib/rustlib/src/rust/library/std/src/lib.rs";
    assert_eq!(
        scan_bytes(&object(system), TRUST),
        vec![Evidence::RustupPath(
            "1.97.1-x86_64-unknown-linux-gnu".to_string()
        )]
    );
    // Not rooted at a directory boundary: a longer word ending in `rustup` is no such path.
    assert!(scan_bytes(&object(b"/opt/myrustup/toolchains/x/lib"), TRUST).is_empty());
}

#[test]
fn evidence_straddling_a_read_boundary_is_found_once_and_the_digest_is_whole() {
    // A small step, so every boundary case is a few KiB rather than 8 MiB of debug-build
    // hashing: the anchor just inside the scanned window, just past it, and in the tail.
    const STEP: usize = 4 * TAIL;
    let path = format!("/rustc/{STOCK}/library/core/src/panic.rs");
    for offset in [
        STEP - TAIL - 3,
        STEP - TAIL - 1,
        STEP - TAIL,
        STEP - 20,
        STEP - 1,
        STEP + 5,
        3 * STEP - TAIL,
    ] {
        let mut bytes = vec![b'x'; offset];
        bytes[..MACHO.len()].copy_from_slice(MACHO);
        bytes.extend_from_slice(path.as_bytes());
        bytes.extend(std::iter::repeat_n(b'x', 2 * STEP));
        let (hits, digest) = scan_stream(bytes.as_slice(), TRUST, true, STEP).expect("scan");
        assert_eq!(
            hits.get(&Evidence::StdPath(STOCK.to_string())),
            Some(&1),
            "offset {offset}: {hits:?}"
        );
        assert_eq!(
            digest.as_deref(),
            Some(aterm_codec::hex::encode_lower(&Sha256::digest(&bytes)).as_str()),
            "offset {offset}"
        );
    }
}

#[test]
fn find_byte_agrees_with_a_linear_search() {
    let mut hay: Vec<u8> = (0..4096u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    for needle in [b'r', 0, 0x80, 0xff] {
        for start in 0..40 {
            let want = hay[start..].iter().position(|&b| b == needle);
            assert_eq!(
                find_byte(&hay[start..], needle),
                want,
                "needle {needle} start {start}"
            );
        }
    }
    hay.iter_mut().for_each(|b| *b = b'x');
    assert_eq!(find_byte(&hay, b'r'), None);
}

#[test]
fn scan_tree_reads_compiled_output_only() {
    let (_dir, d) = tmp("tree");
    std::fs::create_dir_all(d.join("bin")).expect("mkdir");
    std::fs::create_dir_all(d.join("lib/rustlib/t/lib")).expect("mkdir");
    let stock = format!("/rustc/{STOCK}/library/std/src/x.rs");
    std::fs::write(d.join("bin/ay"), object(stock.as_bytes())).expect("write");
    std::fs::write(d.join("bin/clean"), object(b"nothing foreign")).expect("write");
    // A text file that quotes a stock path is documentation, not compiled output.
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
    let report = scan_tree(&d, TRUST).expect("scan");
    assert_eq!(report.objects, 3);
    let files: Vec<&str> = report.findings.iter().map(|f| f.file.as_str()).collect();
    assert_eq!(files, vec!["bin/ay", "lib/rustlib/t/lib/libstd-1.rlib"]);
}

// Compiler admission.

fn trust_vv(commit: &str) -> String {
    format!(
        "rustc 1.99.0-dev ({} 2026-09-23) (trustc 0.1.0)\nbinary: trustc\ncommit-hash: {commit}\n\
         commit-date: 2026-09-23\nhost: aarch64-apple-darwin\nrelease: 1.99.0-dev\ntrust: 0.1.0\n\
         LLVM version: 22.1.2\n",
        &commit[..9]
    )
}

const STOCK_VV: &str = "rustc 1.97.1 (8bab26f4f 2026-07-14)\nbinary: rustc\ncommit-hash: \
     8bab26f4f68e0e26f0bb7960be334d5b520ea452\ncommit-date: 2026-07-14\nhost: \
     aarch64-apple-darwin\nrelease: 1.97.1\nLLVM version: 20.1.8\n";

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

fn never_run(_: &Path) -> Result<String, String> {
    panic!("a refused compiler must be refused before it is executed")
}

fn sha(path: &Path) -> String {
    crate::tree::sha256_file(path).expect("hash")
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
fn a_script_trustc_is_refused_before_it_runs() {
    let (_dir, d) = tmp("script");
    let bin = toolchain(&d, b"");
    std::fs::write(
        bin.join("trustc"),
        b"#!/bin/sh\nexec \"$HOME/.cargo/bin/rustc\" \"$@\"\n",
    )
    .expect("write");
    let err = check_compiler_with(&bin, LineagePolicy::Dormant, &never_run).expect_err("a script");
    assert!(
        err.iter().any(|l| l.contains("interpreter script")),
        "{err:?}"
    );
}

/// Sh wrappers named trustc/targo that exec rustup's stable and sed its identity into Trust's.
#[test]
fn a_genesis_style_wrapper_is_refused_by_content() {
    let (_dir, d) = tmp("genesis");
    let bin = toolchain(&d, b"");
    for (name, real) in [("trustc", "rustc"), ("targo", "cargo")] {
        std::fs::write(
            bin.join(name),
            format!(
                "#!/bin/sh\nreal=\"/Users//x/.cargo/bin/{real}\"\ncase \"$1\" in -vV|--version) \
                 \"$real\" \"$@\" | sed 's/^{real} /t{real} /'; exit ;; esac\nexec \"$real\" \"$@\"\n"
            ),
        )
        .expect("write");
    }
    let err = check_compiler_with(&bin, LineagePolicy::Dormant, &never_run).expect_err("genesis");
    assert_eq!(
        err.iter()
            .filter(|l| l.contains("interpreter script"))
            .count(),
        2,
        "{err:?}"
    );
}

/// A stock rustc copied in under Trust's names: native, so it runs, and its `-vV` and its
/// driver's bytes say what it is.
#[test]
fn a_renamed_stock_rustc_is_refused_by_content() {
    let (_dir, d) = tmp("renamed");
    let stock_std = format!("/rustc/{STOCK}/library/std/src/panicking.rs");
    let bin = toolchain(&d, stock_std.as_bytes());
    let err = check_compiler_with(
        &bin,
        LineagePolicy::Dormant,
        &probe_says(STOCK_VV.to_string()),
    )
    .expect_err("stock");
    assert!(
        err.iter().any(|l| l.contains("no `trust:` line")),
        "{err:?}"
    );
    assert!(err.iter().any(|l| l.contains("binary: rustc")), "{err:?}");
    // Even with a Trust-shaped report, a driver carrying another compiler's std is not
    // this compiler's.
    let err = check_compiler_with(&bin, LineagePolicy::Dormant, &probe_says(trust_vv(TRUST)))
        .expect_err("stock bytes");
    assert!(
        err.iter()
            .any(|l| l.contains("own bytes carry") && l.contains(STOCK)),
        "{err:?}"
    );
}

#[test]
fn a_stock_location_is_refused_before_it_runs() {
    let (_dir, d) = tmp("rustup-home");
    let root = d.join(".rustup/toolchains/stable-aarch64-apple-darwin");
    let bin = toolchain(&root, b"");
    let err = check_compiler_with(&bin, LineagePolicy::Dormant, &never_run).expect_err("rustup");
    assert!(
        err.iter().any(|l| l.contains("rustup's directory")),
        "{err:?}"
    );
    // A system RUSTUP_HOME's toolchain (no dot: /usr/local/rustup/toolchains/<name>).
    let (_dir3, d3) = tmp("system-rustup");
    let bin3 = toolchain(
        &d3.join("rustup/toolchains/1.97.1-x86_64-unknown-linux-gnu"),
        b"",
    );
    let err =
        check_compiler_with(&bin3, LineagePolicy::Dormant, &never_run).expect_err("system rustup");
    assert!(
        err.iter().any(|l| l.contains("rustup's directory")),
        "{err:?}"
    );
    // A trustc linked into ~/.cargo (a rustup proxy) resolves there and is refused too.
    let (_dir2, d2) = tmp("cargo-link");
    let bin2 = toolchain(&d2.join("tc"), b"");
    std::fs::create_dir_all(d2.join(".cargo/bin")).expect("mkdir");
    std::fs::write(d2.join(".cargo/bin/rustc"), object(b"proxy")).expect("write");
    std::fs::remove_file(bin2.join("trustc")).expect("rm");
    #[cfg(unix)]
    std::os::unix::fs::symlink(d2.join(".cargo/bin/rustc"), bin2.join("trustc")).expect("link");
    #[cfg(unix)]
    {
        let err =
            check_compiler_with(&bin2, LineagePolicy::Dormant, &never_run).expect_err("proxy");
        assert!(err.iter().any(|l| l.contains("~/.cargo")), "{err:?}");
    }
}

#[test]
fn the_version_report_must_be_a_trust_self_report() {
    assert!(parse_version_verbose(&trust_vv(TRUST)).is_ok());
    let wrong_first = trust_vv(TRUST).replacen(" (trustc 0.1.0)", "", 1);
    assert!(parse_version_verbose(&wrong_first).is_err());
    let mismatched = trust_vv(TRUST).replacen("7d5533b2b", "321aaeda7", 1);
    let err = parse_version_verbose(&mismatched).expect_err("mismatch");
    assert!(err.iter().any(|l| l.contains("321aaeda7")), "{err:?}");
    assert!(parse_version_verbose(&trust_vv(TRUST).replace("trust: 0.1.0\n", "")).is_err());
}

#[test]
fn no_receipt_is_refused_unless_dormant() {
    let (_dir, d) = tmp("no-receipt");
    let bin = toolchain(&d, b"");
    let probe = probe_says(trust_vv(TRUST));
    let err = check_compiler_with(&bin, LineagePolicy::Require, &probe).expect_err("unproven");
    assert!(
        err.iter().any(|l| l.contains("lineage is unproven")),
        "{err:?}"
    );
    let admitted = check_compiler_with(&bin, LineagePolicy::Dormant, &probe).expect("dormant");
    assert_eq!(admitted.lineage, LineageProof::Unproven);
    let record: BTreeMap<_, _> = admitted.record().into_iter().collect();
    assert_eq!(record["lineage"], "unproven");
    assert_eq!(record["compiler_commit"], TRUST);
    assert_eq!(record["lineage_receipt_sha256"], "none");
}

#[test]
fn a_seed_mint_admits_the_bytes_it_compared_and_nothing_else() {
    let (_dir, d) = tmp("seed-mint");
    let bin = toolchain(&d, b"");
    let probe = probe_says(trust_vv(TRUST));
    seed_mint(&d, TRUST, &[]);
    let admitted = check_compiler_with(&bin, LineagePolicy::Require, &probe).expect("admitted");
    assert!(
        matches!(admitted.lineage, LineageProof::SeedMint { ref relation, .. } if relation == "pin-ancestor")
    );

    // One flipped digest: the receipt no longer describes this seal.
    let text = std::fs::read_to_string(d.join(SEED_MINT_FILE)).expect("read");
    let trustc_sha = sha(&d.join("bin/trustc"));
    let first = if trustc_sha.starts_with('0') {
        "1"
    } else {
        "0"
    };
    let flipped = text.replace(&trustc_sha, &format!("{first}{}", &trustc_sha[1..]));
    std::fs::write(d.join(SEED_MINT_FILE), flipped).expect("write");
    let err = check_compiler_with(&bin, LineagePolicy::Require, &probe).expect_err("flipped");
    assert!(
        err.iter()
            .any(|l| l.contains("does not describe this seal") && l.contains("bin/trustc")),
        "{err:?}"
    );
    // Dormancy never excuses a wrong receipt.
    assert!(check_compiler_with(&bin, LineagePolicy::Dormant, &probe).is_err());

    // A receipt that compared other files does not cover this compiler's bytes.
    seed_mint(&d, TRUST, &[]);
    let text = std::fs::read_to_string(d.join(SEED_MINT_FILE)).expect("read");
    let only_trustc: String = text
        .lines()
        .filter(|l| !l.contains("librustc_driver"))
        .map(|l| format!("{l}\n"))
        .collect();
    std::fs::write(d.join(SEED_MINT_FILE), only_trustc).expect("write");
    let err = check_compiler_with(&bin, LineagePolicy::Require, &probe).expect_err("uncovered");
    assert!(
        err.iter()
            .any(|l| l.contains("does not cover this compiler")),
        "{err:?}"
    );

    // A receipt minted for another compiler.
    seed_mint(&d, "4444444444444444444444444444444444444444", &[]);
    let err = check_compiler_with(&bin, LineagePolicy::Require, &probe).expect_err("other");
    assert!(
        err.iter().any(|l| l.contains("was minted for 4444")),
        "{err:?}"
    );
}

/// Each SEED-MINT defect, alone, is exactly one refusal, and it names that defect.
#[test]
fn each_seed_mint_defect_alone_is_one_refusal_naming_it() {
    let (_dir, d) = tmp("mint-words");
    toolchain(&d, b"");
    #[cfg(unix)]
    std::os::unix::fs::symlink("../bin/trustc", d.join("lib/trustc-link")).expect("link");
    let only = |kv: &[(&str, &str)]| {
        seed_mint(&d, TRUST, kv);
        #[cfg(unix)]
        {
            let mut t = std::fs::read_to_string(d.join(SEED_MINT_FILE)).expect("read");
            t.push_str("compiler_link=../bin/trustc lib/trustc-link\n");
            std::fs::write(d.join(SEED_MINT_FILE), t).expect("write");
        }
        let r = seed_mint_refusals(&d, TRUST);
        assert!(r.len() <= 1, "one change, one refusal: {r:?}");
        r.into_iter().next().unwrap_or_default()
    };
    assert_eq!(only(&[]), "");
    assert!(only(&[("source_commit", "4444444444444444444444444444444444444444")])
        .starts_with(&format!("SEED-MINT was minted for 4444444444444444444444444444444444444444, not for this seal's compiler {TRUST}")));
    assert!(
        only(&[("ddc_seed_relation", "reproducibility-control")])
            .starts_with("SEED-MINT records a reproducibility control, not a double compile")
    );
    assert_eq!(
        only(&[("ddc_seed_relation", "ddc")]),
        "SEED-MINT records no double compile against another compiler (ddc_seed_relation 'ddc')"
    );
    assert!(
        only(&[("ddc_verdict", "DIFFERENT")]).starts_with(
            "SEED-MINT does not record byte-identical stage2 trees from the two seeds"
        )
    );
    // The double compile's substance, not its verdict word: two seeds that built
    // different stage2 trees are refused even beside `ddc_verdict=identical`, and so is a
    // pair of equal digests that are no sha256.
    let other_tree = "b".repeat(64);
    assert_eq!(
        only(&[("stage2_tree_sha256_ddc_seed", other_tree.as_str())]),
        format!(
            "SEED-MINT does not record byte-identical stage2 trees from the two seeds (verdict \
             'identical', pinned seed {}, DDC seed {other_tree})",
            "a".repeat(64)
        )
    );
    assert!(
        only(&[
            ("stage2_tree_sha256_pinned_seed", "tree"),
            ("stage2_tree_sha256_ddc_seed", "tree")
        ])
        .starts_with("SEED-MINT does not record byte-identical stage2 trees from the two seeds")
    );
    assert!(
        only(&[("repin_compiler_git_commit_hash", "")])
            .starts_with("SEED-MINT records no re-pin of src/stage0 to this compiler")
    );
    assert!(
        only(&[("schema", "v0")])
            .starts_with("SEED-MINT is not a trust-seed-mint-v1 receipt (schema 'v0')")
    );
    // A compared file that is not the seal's.
    seed_mint(&d, TRUST, &[]);
    std::fs::write(d.join("bin/trustc"), object(b"swapped")).expect("write");
    let r = seed_mint_refusals(&d, TRUST);
    assert_eq!(
        r,
        vec!["SEED-MINT does not describe this seal: 1 of its 2 compared entries are missing or differ here — bin/trustc".to_string()]
    );
    // A compared link repointed after the cut.
    #[cfg(unix)]
    {
        seed_mint(&d, TRUST, &[]);
        let mut t = std::fs::read_to_string(d.join(SEED_MINT_FILE)).expect("read");
        t.push_str("compiler_link=../bin/trustc lib/trustc-link\n");
        std::fs::write(d.join(SEED_MINT_FILE), t).expect("write");
        std::fs::remove_file(d.join("lib/trustc-link")).expect("rm");
        std::os::unix::fs::symlink("../bin/targo", d.join("lib/trustc-link")).expect("link");
        let r = seed_mint_refusals(&d, TRUST);
        assert_eq!(r.len(), 1, "{r:?}");
        assert!(r[0].ends_with("— lib/trustc-link"), "{r:?}");
    }
    // A compared path that climbs out of the seal is never read.
    seed_mint(&d, TRUST, &[]);
    let mut t = std::fs::read_to_string(d.join(SEED_MINT_FILE)).expect("read");
    t.push_str(&format!("compiler_file={} ../outside\n", "b".repeat(64)));
    std::fs::write(d.join(SEED_MINT_FILE), t).expect("write");
    assert!(
        seed_mint_refusals(&d, TRUST)
            .iter()
            .any(|l| l.contains("../outside"))
    );
    // No receipt at all.
    std::fs::remove_file(d.join(SEED_MINT_FILE)).expect("rm");
    assert!(seed_mint_refusals(&d, TRUST)[0].starts_with("the seal carries no SEED-MINT"));
}

/// The bundle's round trip: a seal admitted by SEED-MINT is staged, its bytes change, LINEAGE
/// is stamped over the shipped bytes and admits them, and an edit to the record or a byte it
/// names is refused.
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
    let err = stamp_with(&stage, &seal_bin, &stage_says_other).expect_err("another commit");
    assert!(
        err.iter()
            .any(|l| l.contains(&format!("the staged bin/trustc reports {other}"))),
        "{err:?}"
    );
    seed_mint(&stage, TRUST, &[("pinned_seed_commit", other)]);
    let err = stamp_with(&stage, &seal_bin, &probe).expect_err("another staged receipt");
    assert!(
        err.iter().any(|l| l.contains("the staged SEED-MINT is")),
        "{err:?}"
    );
    assert!(!stage.join(LINEAGE_FILE).exists());
    std::fs::copy(seal.join(SEED_MINT_FILE), stage.join(SEED_MINT_FILE)).expect("restore");
    let text = stamp_with(&stage, &seal_bin, &probe).expect("stamp");
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
    // Swap the driver: nothing but the record's digest names the driver a stamped tree runs.
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
    // Replace SEED-MINT with another receipt of the same commit that passes every check of
    // its own: the record is bound to the receipt it was stamped against.
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
        aterm_codec::hex::encode_lower(&Sha256::digest(&stamped_mint))
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
    assert!(stamp_with(&stage, &seal_bin, &probe).is_err());
    assert!(!stage.join(LINEAGE_FILE).exists());
}
