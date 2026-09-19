// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The atpkg pack lanes' ARCHITECTURE claim, enforced — and enforced ON EVERY HOST.
//!
//! `tools/atpkg-pack.sh` writes `target = "<triple>"` into a manifest it then signs, and
//! every client picks its artifact by that field. A signature proves the bytes were not
//! altered; it says nothing about whether they are the architecture they claim. The lane
//! has carried an arch gate for that since the macOS mislabel — under the comment "never
//! trust the label, read the Mach-O" — but until 2026-09-18 it was written
//!
//! ```text
//! if [[ -n "$WANT_ARCH" ]] && command -v lipo >/dev/null 2>&1; then …
//! ```
//!
//! and `lipo` ships with Xcode, on macOS, only. On the Linux publisher — the lane
//! `tools/linux-auto-atpkg.sh` drives with `TRIPLE="$LINUX_TRIPLE"` — and on any Windows
//! one, THE WHOLE LOOP WAS SKIPPED WITH NO MESSAGE. Measured on x86_64 Linux by handing
//! the shipping script an AArch64 ELF with `TRIPLE=x86_64-unknown-linux-gnu`: it packed,
//! wrote `pkg-stubprog-1.toml` claiming `target = "x86_64-unknown-linux-gnu"` over a
//! payload that cannot exec on one, printed `PACK-SPEC` and exited 0. The `*)  WANT_ARCH=""`
//! arm was a second such skip, for any triple the `case` did not name.
//!
//! `tools/atpkg-pack-bundle.sh` was worse: it packs a PRE-BUILT sysroot, has no `--target`
//! anywhere to fall back on, and had NO arch gate on any OS — only fat-Mach-O thinning
//! under `case "$TRIPLE" in *-apple-darwin)`, plus a per-bin smoke-exec that runs the bins
//! NATIVELY on the builder and so passes for whatever the builder is (the same shape as the
//! Linux release lane's `aterm --version`, which is how that lane shipped an AArch64 tarball
//! under an x86_64 name).
//!
//! A guard that cannot fail is worse than no guard, because it reads as coverage. So the
//! reader is now `od` over ELF, Mach-O (thin and fat) and PE — coreutils, present wherever
//! bash is — and a missing reader, an unnameable triple and an empty file list are each a
//! REFUSAL. This file is the half of that guard which runs where bash does not: pure text
//! over committed files plus `atpkg::TARGETS`. The BEHAVIOUR is exercised against crafted
//! object headers, and end to end against the shipping packer, by
//! `tools/test-atpkg-pack-arch-gate.sh`, which `the_hermetic_suite_passes` below runs
//! wherever there is a shell to run it in.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(rel: &str) -> String {
    let p = repo_root().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{rel} is readable: {e}"))
}

/// The file with every whole-line comment removed. These headers legitimately QUOTE the
/// defect they replaced — `command -v lipo`, `WANT_ARCH=""` — while explaining why it is
/// gone, so a law about what the code SPELLS has to look at the code.
fn code(text: &str) -> String {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Byte offset of `needle`, with a named failure — `packlane_order.rs`'s technique: in a
/// shell script ORDER is the property, and source order is where it lives.
fn at(text: &str, needle: &str) -> usize {
    text.find(needle)
        .unwrap_or_else(|| panic!("the pack lane lost its {needle:?} marker"))
}

/// THE DEFECT ITSELF: the binary lane's arch gate may never again be conditional on a
/// tool that exists on one of the three operating systems aterm publishes from.
#[test]
fn the_binary_lanes_arch_gate_is_not_conditional_on_the_host() {
    let s = code(&read("tools/atpkg-pack.sh"));
    assert!(
        !s.contains("lipo"),
        "tools/atpkg-pack.sh gates on `lipo` again — that tool is macOS-only, and the \
         gate it guards then does NOTHING on the Linux and Windows publishers while \
         reading as coverage"
    );
    assert!(
        s.contains("atpkg_arch_gate \"atpkg-pack\" \"$TRIPLE\" \"${EXES[@]}\""),
        "tools/atpkg-pack.sh no longer runs the portable arch gate over the binaries it \
         is about to sign a target claim for"
    );
    // The `*) WANT_ARCH=""` arm was the second skip: a triple the table did not name
    // produced an empty want and the loop never ran. The claim is resolved in the
    // library now, where an unnameable triple is a refusal.
    assert!(
        !s.contains("WANT_ARCH"),
        "the `WANT_ARCH` table is back in tools/atpkg-pack.sh — its `*)` arm meant \
         'nothing to check' for every triple it did not name"
    );
    // The gate must stand BEFORE the pack stages, tars or signs anything: a refusal
    // after the signature is a published mislabel.
    let gate = at(&s, "atpkg_arch_gate \"atpkg-pack\"");
    assert!(
        gate < at(&s, "cp \"$exe\" \"$STAGE/root/bin/$b\""),
        "the arch gate must run before the binaries are staged"
    );
    assert!(
        gate < at(&s, "ASSET="),
        "the arch gate must run before the tarball is named and built"
    );
}

/// The bundle lane packs a pre-built sysroot and has no `--target` to fall back on, so
/// its arch gate is the ONLY thing standing between its payload and its label.
#[test]
fn the_bundle_lane_reads_the_payload_it_signs_a_triple_for() {
    let raw = read("tools/atpkg-pack-bundle.sh");
    let s = code(&raw);
    assert!(
        s.contains("atpkg_arch_gate_tree \"atpkg-pack-bundle\" \"$TRIPLE\" \"$STAGE/root/bin\""),
        "tools/atpkg-pack-bundle.sh does not read its staged bin/ against the triple it \
         signs — and it has no --target anywhere to catch a wrong one for it"
    );
    // After the last byte mutation (so what is gated is what ships) and before
    // tar/sha256/tree_root/sign (so the signed manifest describes gated bytes) — the
    // §14 invariant `packlane_order.rs` holds for every other pass in this lane. Offsets
    // are taken over the RAW script, whose `# --- …` section heads are the markers that
    // lane's order has always been pinned by.
    let relocate = at(&raw, "\"$ATPKG\" relocate");
    let arch = at(&raw, "atpkg_arch_gate_tree");
    let tar = at(&raw, "# --- tar.zst with the SAME hygiene");
    assert!(
        relocate < arch,
        "the arch gate must read the FINAL relocated bytes, not the pre-relocation stage"
    );
    assert!(
        arch < tar,
        "the arch gate must refuse BEFORE the tarball, its sha256 and the signature"
    );
}

/// Both lanes must resolve the gate from the one shared implementation: a second copy is
/// how two dialects of one refusal get written, and how one of them rots unnoticed.
#[test]
fn there_is_exactly_one_arch_reader_and_both_lanes_use_it() {
    let lib = read("tools/atpkg-publish-lib.sh");
    for f in ["atpkg_arch_expect", "atpkg_arch_read", "atpkg_arch_gate"] {
        assert!(
            lib.contains(&format!("{f}() {{")),
            "tools/atpkg-publish-lib.sh no longer defines {f}"
        );
    }
    // The reader reads all three object formats aterm ships, by header, with `od` — and
    // `od -v`, because without it repeated identical lines collapse to `*` and a
    // zero-padded header (every PE has 58 such bytes) comes back short.
    let lib_code = code(&lib);
    for marker in [
        "\\x7fELF",
        "MH_MAGIC_64",
        "FAT_MAGIC",
        "PE\\0\\0",
        "od -An -v",
    ] {
        assert!(
            lib_code.contains(marker) || lib.contains(marker),
            "the arch reader lost its {marker:?} arm"
        );
    }
    // rc 2 is "this machine has no reader": the arm the old gate took silently on every
    // Linux box. It must reach a refusal, never a skip.
    assert!(
        lib_code.contains("return 2"),
        "the reader no longer distinguishes 'no reader on this machine' from 'unreadable \
         file' — that distinction is what makes a missing tool a refusal"
    );
    for lane in ["tools/atpkg-pack.sh", "tools/atpkg-pack-bundle.sh"] {
        let s = read(lane);
        assert!(
            s.contains("atpkg-publish-lib.sh"),
            "{lane} must source the shared library that holds the one arch reader"
        );
    }
}

/// Every triple aterm SHIPS must be a triple the gate can name. `atpkg_arch_expect`
/// refuses what it cannot name (deliberately — the old `*) WANT_ARCH=""` default was a
/// silent skip), so a target added to [`atpkg::TARGETS`] without an arm here would make
/// its own pack lane refuse. The hermetic suite asserts the mapping for each one; this
/// holds that list and the roster equal.
#[test]
fn the_gate_can_name_every_shipped_triple() {
    let suite = read("tools/test-atpkg-pack-arch-gate.sh");
    for t in atpkg::TARGETS {
        assert!(
            suite.contains(&format!("atpkg_arch_expect {t})")),
            "{t} is in atpkg::TARGETS but tools/test-atpkg-pack-arch-gate.sh never asks \
             the gate what it expects for it — an unnamed triple is a refused pack"
        );
    }
    let lib = read("tools/atpkg-publish-lib.sh");
    for fmt in ["macho", "elf", "pe"] {
        assert!(
            lib.contains(&format!("fmt={fmt}")),
            "atpkg_arch_expect lost its {fmt} arm"
        );
    }
}

/// The behavioural half, where a shell exists: `tools/test-atpkg-pack-arch-gate.sh` drives
/// the shipping library against crafted ELF/Mach-O/PE headers, and the shipping
/// `tools/atpkg-pack.sh` end to end against a foreign-arch binary.
#[test]
fn the_hermetic_suite_passes() {
    let suite = repo_root().join("tools/test-atpkg-pack-arch-gate.sh");
    assert!(
        suite.is_file(),
        "tools/test-atpkg-pack-arch-gate.sh is gone — the behavioural half of this guard \
         cannot vanish quietly"
    );
    let out = std::process::Command::new("bash")
        .arg(&suite)
        .current_dir(repo_root())
        .output()
        .expect("bash runs the hermetic suite");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stdout.contains("test-atpkg-pack-arch-gate: PASS"),
        "the pack lanes' arch suite failed:\n--- stdout ---\n{stdout}\
         \n--- stderr ---\n{stderr}"
    );
}
