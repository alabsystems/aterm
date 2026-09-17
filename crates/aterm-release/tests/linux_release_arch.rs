// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Linux release lane's ARCHITECTURE claim, enforced.
//!
//! `tools/linux-auto-release.sh` builds the Linux tarball HOST-NATIVE — it must, because
//! `.cargo/config.toml`'s COROLLARY measures that pinning `--target` to the native triple
//! withholds the `cfg(trust_verify)` table's rustflags from host units and fails the
//! build. So the architecture of what it ships is not a choice the script makes; it is a
//! fact about the machine the timer fired on, and every arch-bearing name it writes is a
//! CLAIM about that fact.
//!
//! Until 2026-09-16 the claim was a constant. The asset name was literally
//! `aterm-<v>-linux-x86_64.tar.gz`, the release-notes heading literally `## Linux
//! x86_64`, and the only gate on the binary was `aterm --version` — which runs natively
//! on the builder and therefore passes for whatever the builder is. On an aarch64 Linux
//! builder that lane produced an AArch64 ELF, packaged it under the x86_64 name, minted a
//! `.sha256` over those bytes and uploaded it to BOTH release repos. Every downstream
//! integrity check passed, because a digest covers the bytes and never the claim about
//! them — the same lesson `tools/atpkg-pack.sh` learned on the macOS side and answered
//! with `lipo`. Users on the architecture named would have installed a binary that cannot
//! exec, and no one working on an x86_64 box could see any of it.
//!
//! This file is the half of the guard that runs where bash does not: it is pure text over
//! committed files (`CARGO_MANIFEST_DIR` only — no network, no subprocess, no toolchain),
//! so it compiles and runs on every target aterm ships. The BEHAVIOUR — the arch table,
//! the ELF reader, the gate's refusals — is exercised against crafted ELF headers by
//! `tools/test-linux-auto-release.sh`, which `the_hermetic_suite_passes` below runs
//! wherever there is a shell to run it in.

use std::path::{Path, PathBuf};

/// The two architectures aterm ships a Linux tarball for: `uname -m`'s answer, the asset
/// label that must appear in the name, and the target triple the release notes name.
const SHIPPED: [(&str, &str, &str); 2] = [
    ("x86_64", "x86_64", "x86_64-unknown-linux-gnu"),
    ("aarch64", "aarch64", "aarch64-unknown-linux-gnu"),
];

/// The names this lane published before the fix, and therefore forever after for x86_64:
/// live release URLs, `tools/install.sh`'s lookup and the notes heading a reader scrolls
/// to. The fix derives them; it must derive them to exactly these bytes.
const HISTORICAL_X86_64_ASSET: &str = "aterm-0.64.0-linux-x86_64.tar.gz";
const HISTORICAL_X86_64_HEADING: &str = "## Linux x86_64";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(rel: &str) -> String {
    let p = repo_root().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{rel} is readable: {e}"))
}

fn lane() -> String {
    read("tools/linux-auto-release.sh")
}

/// The script with every whole-line comment removed. The header block legitimately QUOTES
/// the old constants while explaining why they are gone, so a law about what the code
/// spells has to look at the code.
fn code(text: &str) -> Vec<(usize, String)> {
    text.lines()
        .enumerate()
        .map(|(i, l)| (i + 1, l.to_string()))
        .filter(|(_, l)| !l.trim_start().starts_with('#'))
        .collect()
}

/// Byte offset of `needle`, with a named failure — the `packlane_order.rs` technique: in
/// a shell script, ORDER is the property, and source order is where it lives.
fn at(text: &str, needle: &str) -> usize {
    text.find(needle)
        .unwrap_or_else(|| panic!("the Linux release lane lost its {needle:?} marker"))
}

/// The single-quoted-into-shell template a one-line helper echoes, e.g. the body
/// `echo "aterm-$1-linux-$2.tar.gz"` of `tarball_name`. Extracted rather than retyped, so
/// the assertions below are about the shipping file and not about a copy of it.
fn echo_template(text: &str, func: &str) -> String {
    let body = &text[at(text, &format!("{func}() {{"))..];
    let line = body
        .lines()
        .find(|l| l.trim_start().starts_with("echo \""))
        .unwrap_or_else(|| panic!("{func} no longer echoes a single template line"));
    let t = line.trim_start().trim_start_matches("echo \"");
    t.strip_suffix('"')
        .unwrap_or_else(|| panic!("{func}'s template is not one quoted string: {line}"))
        .to_string()
}

/// `tarball_name`/`notes_heading` applied to positional arguments, the way bash would.
/// The arity is asserted first: a helper that quietly grew a third parameter would
/// otherwise be "substituted" into a name nothing ever publishes.
fn apply(template: &str, args: &[&str]) -> String {
    for n in 1..=9usize {
        assert_eq!(
            template.contains(&format!("${n}")),
            n <= args.len(),
            "the template {template:?} does not take exactly {} positional argument(s)",
            args.len()
        );
    }
    let mut out = template.to_string();
    for (i, a) in args.iter().enumerate() {
        let n = i + 1;
        out = out.replace(&format!("${n}"), a);
    }
    out
}

#[test]
fn no_arch_bearing_name_is_a_constant_any_more() {
    let s = lane();
    for (line, text) in code(&s) {
        for forbidden in ["linux-x86_64", "linux-aarch64"] {
            assert!(
                !text.contains(forbidden),
                "tools/linux-auto-release.sh:{line} spells {forbidden:?} as a literal: \
                 {text:?}. Every asset name in this lane must come from `tarball_name` \
                 with the label `arch_row` derived from `uname -m`, or the builder's \
                 architecture and the name it publishes can disagree again."
            );
        }
        for forbidden in [HISTORICAL_X86_64_HEADING, "## Linux aarch64"] {
            assert!(
                !text.contains(forbidden),
                "tools/linux-auto-release.sh:{line} writes the notes heading {forbidden:?} \
                 as a literal: {text:?}. It must come from `notes_heading`, which is also \
                 what the idempotency check greps for."
            );
        }
    }
}

#[test]
fn the_builders_architecture_is_measured_and_an_unshipped_one_refuses() {
    let s = lane();
    // Measured, not assumed, and measured ONCE: `uname -m` feeds `arch_row`, whose row
    // is unpacked into the ARCH/TRIPLE every name below is written from.
    assert!(
        s.contains(r#"HOST_MACHINE="$(uname -m)""#),
        "the lane no longer asks the machine what it is"
    );
    assert!(
        s.contains(r#"ARCH_ROW="$(arch_row "$HOST_MACHINE")""#),
        "the lane no longer maps `uname -m` through the arch table"
    );
    assert!(
        s.contains("read -r ARCH TRIPLE _ <<<\"$ARCH_ROW\""),
        "ARCH/TRIPLE no longer come from the arch table's row"
    );
    // Both shipped machines, with their triples — and the ELF machine the gate reads.
    for (uname, label, triple) in SHIPPED {
        let row = format!("echo \"{label} {triple} ");
        assert!(
            s.contains(&row),
            "the arch table has no row producing {row:?} — aterm ships {triple}"
        );
        assert!(
            s.contains(&format!("{uname} |")) || s.contains(&format!("| {uname})")),
            "the arch table no longer recognises `uname -m` = {uname}"
        );
    }
    // EM_X86_64 = 0x3e = 62, EM_AARCH64 = 0xb7 = 183 (ELF gABI). A wrong number here is
    // a gate that refuses every honest build or passes every dishonest one.
    assert!(s.contains("x86_64-unknown-linux-gnu 62"), "EM_X86_64 is 62");
    assert!(
        s.contains("aarch64-unknown-linux-gnu 183"),
        "EM_AARCH64 is 183"
    );
    // Anything else is a refusal, never a silent default to x86_64.
    let table = &s[at(&s, "arch_row() {")..];
    let end = table.find("\n}\n").expect("arch_row is a function");
    assert!(
        table[..end].contains("*) return 1 ;;"),
        "the arch table stopped refusing machines aterm does not ship for"
    );
    assert!(
        s.contains("which aterm does not ship a Linux tarball for"),
        "the refusal for an unshipped builder lost its explanation"
    );
}

#[test]
fn the_x86_64_spelling_this_lane_already_published_is_byte_identical() {
    let s = lane();
    let asset = echo_template(&s, "tarball_name");
    let heading = echo_template(&s, "notes_heading");
    assert_eq!(
        apply(&asset, &["0.64.0", "x86_64"]),
        HISTORICAL_X86_64_ASSET,
        "the derived x86_64 asset name moved — every published URL and \
         tools/install.sh's lookup read the old one"
    );
    assert_eq!(
        apply(&heading, &["x86_64"]),
        HISTORICAL_X86_64_HEADING,
        "the derived x86_64 notes heading moved"
    );
    // The consumer side of the same string: the installer's Linux lane asks the release
    // for this exact name. Producer and consumer cannot drift while this holds.
    let installer = read("tools/install.sh");
    let consumed = apply(&asset, &["${TAG#v}", "x86_64"]);
    assert!(
        installer.contains(&consumed),
        "tools/install.sh no longer looks up {consumed:?} — the name the release lane \
         publishes for x86_64 and the name the installer fetches have diverged"
    );
    // And the second architecture gets its own name rather than colliding on the first.
    assert_ne!(
        apply(&asset, &["0.64.0", "x86_64"]),
        apply(&asset, &["0.64.0", "aarch64"]),
        "both architectures resolve to one asset name"
    );
    assert_eq!(
        apply(&asset, &["0.64.0", "aarch64"]),
        "aterm-0.64.0-linux-aarch64.tar.gz"
    );
}

#[test]
fn the_arch_gate_reads_the_elf_before_anything_leaves_the_machine() {
    let s = lane();
    // It reads the header with coreutils `od`, deliberately: a gate that skips itself on
    // a builder without binutils is not a gate, and this one is the only thing standing
    // between a mislabelled binary and two release repos.
    let reader = &s[at(&s, "elf_machine() {")..];
    let end = reader.find("\n}\n").expect("elf_machine is a function");
    assert!(
        reader[..end].contains("od -An -tu1 -N20"),
        "the ELF reader stopped reading the header with od"
    );
    assert!(
        !reader[..end].contains("readelf") && !reader[..end].contains("file -"),
        "the ELF reader now depends on binutils, which a builder may not have — \
         a gate that can be absent is not a gate"
    );
    // It is FATAL. A warning here is an upload that happens anyway.
    let gate = &s[at(&s, "arch_gate() {")..];
    let gate_end = gate.find("\n}\n").expect("arch_gate is a function");
    assert!(
        gate[..gate_end].matches("die").count() >= 3,
        "the arch gate stopped being fatal on a mismatch, an unreadable ELF, or an \
         unknown label"
    );
    // ORDER: the binary is gated after the version gate and BEFORE it is packaged, and
    // the tarball is gated before the first upload — including on the converge path,
    // where the bytes were built by a machine this script never saw.
    let version_gate = at(&s, r#"say "version gate OK"#);
    let bin_gate = at(&s, r#"arch_gate "$BIN" "$ARCH""#);
    let package = at(&s, "PKGROOT=\"$WORK/pkgroot\"");
    let tar_gate = at(&s, r#"arch_gate_tarball "$TAR_PATH" "$ARCH""#);
    let upload = at(&s, r#"upload_missing "$PRIVATE_REPO" gh"#);
    assert!(
        version_gate < bin_gate && bin_gate < package,
        "the binary must be gated after the version gate and before it is packaged"
    );
    assert!(
        tar_gate < upload,
        "the packaged bytes must be gated before the first upload"
    );
    // The version gate alone is what made the defect invisible; say so where it is.
    assert!(
        s.contains("it runs\n\t# NATIVELY on the builder"),
        "the note explaining why `--version` cannot see an arch mismatch is gone"
    );
    // `od` is required up front, beside gh/jq/git/tar: the gate must fail loudly for a
    // missing tool, never quietly for one.
    assert!(
        s.contains("for c in gh jq git tar gzip sha256sum od awk; do"),
        "`od` is no longer a required tool, so the arch gate can be missing its reader"
    );
}

/// The behavioural half, where a shell exists: `tools/test-linux-auto-release.sh` sources
/// the lane through its library seam and drives the arch table, the ELF reader and both
/// gates against ELF headers it writes itself — so the aarch64 cases run on an x86_64 box
/// and the x86_64 cases on an aarch64 one. Hermetic: no network, no token, no cargo.
#[cfg(unix)]
#[test]
fn the_hermetic_suite_passes() {
    let suite = repo_root().join("tools/test-linux-auto-release.sh");
    assert!(
        suite.is_file(),
        "tools/test-linux-auto-release.sh is gone — the behavioural half of this guard \
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
        out.status.success() && stdout.contains("test-linux-auto-release: PASS"),
        "the Linux release lane's arch suite failed:\n--- stdout ---\n{stdout}\
         \n--- stderr ---\n{stderr}"
    );
}
