// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE measured baseline — one place, read by every pinned test.
//!
//! # Why this module exists
//!
//! Every number below is a real measurement of this checkout, taken by the very
//! code the tests exercise (`cargo forge survey`, cross-checked against
//! `tools/forge-budget.tsv`). They stay PINNED on purpose: a graph that moves
//! without anyone deciding it should move is exactly the failure this crate was
//! built to catch, and an assertion is the only thing that notices.
//!
//! They live in ONE file for an equally deliberate reason. The entire point of
//! forge is that the third-party surface SHRINKS. Before this module, every
//! successful extraction — sha2/hmac out, the a11y-accesskit default dropped —
//! reddened fourteen tests across six files and forced four test RENAMES,
//! because the counts were scattered through `loc`, `resolve`, `survey`,
//! `dominator`, `blame` and `check`, and several test names spelled the numbers
//! out (`mac_arm_is_206_packages_53_workspace_153_third_party`). A design where
//! doing the right thing produces a wall of red teaches people to stop reading
//! the red. Updating a baseline after an extraction is now ONE edit here.
//!
//! # These are not the ratchet
//!
//! `tools/forge-budget.tsv` is the ratchet: it enforces that the surface only
//! ever decreases, and `cargo forge budget` is the gate on it. This module is
//! the EQUALITY pin — "the graph is exactly this today" — which catches motion
//! in either direction, including motion the ratchet is happy about but nobody
//! asked for.
//!
//! # Re-measuring
//!
//! ```text
//! cargo run -q -p aterm-forge -- survey          # the per-cell table
//! cargo run -q -p aterm-forge -- blame <name> --cell <cell>   # one dominator
//! ```
//!
//! Copy what the tool prints. Never split the difference with an old number: if
//! a value disagrees with the measurement, the measurement is right and the
//! reason for the change belongs in the commit message.
//!
//! Last measured: 2026-08-28, on the tree that activated FOUR `[patch.crates-io]`
//! replacements at once — `log`, `cfg-if`, `profiling` and `arrayvec`, which are
//! `crates/aterm-log-shim`, `-cfg-if`, `-profiling` and `-arrayvec`.
//!
//! THIS ROUND IS A DIFFERENT SHAPE FROM EVERY ONE BEFORE IT, and the difference
//! is the point. Nothing was rewritten: not one aterm call site changed, because
//! aterm has no call sites on any of the four. All 20 `log` consumers, all 23
//! `cfg-if` consumers, all 3 `profiling` consumers and all 6 `arrayvec`
//! consumers are THIRD-PARTY, so the call-site census that drove every earlier
//! extraction reports zero here and moves on. The patch table is the only lever
//! that reaches them, and the replacements it points at already existed.
//!
//! Every cell moved by EXACTLY THE SAME 10,537 LINES and exactly 4 packages:
//!   mac-arm  1,497,967 -> 1,487,430   105 -> 101 third-party, 62 -> 66 workspace
//!   linux    3,376,578 -> 3,366,041   209 -> 205,             63 -> 67
//!   win      4,001,948 -> 3,991,411   111 -> 107,             61 -> 65
//!   wasm     1,414,616 -> 1,404,079    98 ->  94,             61 -> 65
//! Identical to the line in all four because all four replaced packages are
//! target-independent source. `resolved` did not move at all: each replacement
//! is a 1-for-1 substitution, so a third-party package became a workspace member
//! rather than leaving — the same accounting `aterm-time` produced when it
//! retired `web-time`, four times over.
//!
//! ONE DOMINATOR MOVED, TWICE, and the second move was a BUG IN THE TOOL that
//! this wave was the first graph shape able to expose. `wgpu` went
//! 464,874 -> 462,666 LOC at 33 packages for the honest reason — it holds
//! `arrayvec` and `profiling` in its subtree and their upstream copies were
//! 2,208 lines that our replacements are not — and then to 31 / 460,851 when
//! `dominator::dom_against` was corrected.
//!
//! The correction: a dominator counted EVERY package that falls, and a
//! `[patch.crates-io]` replacement is a first-party workspace member whose only
//! parents are third-party. `crates/aterm-profiling` and `crates/aterm-arrayvec`
//! hang under `wgpu`/`naga` and under nothing else, so blocking `wgpu` removed
//! them too and billed their 1,815 lines of OUR code to wgpu. The survey's own
//! PARTITION CHECK caught it — 37 non-nested rows covering 103 packages /
//! 1,489,245 LOC against a cell holding 101 / 1,487,430 — which is exactly what
//! that check is for, and it is the reason the number in this file is 460,851
//! rather than a plausible 462,666 nobody would have questioned. `dom_against`
//! now skips packages whose facts say `!is_third_party`.
//!
//! Note the direction: the tool's error made a dependency look MORE expensive
//! than it is, i.e. it flattered a future retirement of wgpu. The other
//! measurement trap recorded below (`loc::package_dir` reading our facade as
//! upstream's crate) flattered the campaign the other way. Both are the same
//! class — first-party lines counted as third-party surface — and this file is
//! where that class gets caught.
//!
//! The other four mac-arm anchors and the linux anchor re-measure exactly as
//! pinned: none of them is the sole parent of a patched replacement.
//!
//! A TRAP THIS ROUND FOUND, recorded because it is silent and it will recur:
//! `[patch.crates-io]` does NOT delete the registry's other versions of a name —
//! it hides only the version the patch itself declares, and adds itself as one
//! more candidate. `crates/aterm-arrayvec` shipped as 0.7.6 while its own
//! differential oracle pinned registry `arrayvec =0.7.8`; cargo had to activate
//! a real 0.7.8 for the oracle, then satisfied all six consumers from it too.
//! The patch row replaced NOTHING, `cargo tree -i arrayvec` printed a
//! source-less `v0.7.8` under naga, and CARGO warned about none of it. The shim
//! is 0.7.8 now and the oracle `=0.7.7`. THE RULE: a patch target's version must
//! be >= every other version of that name this workspace forces into the graph,
//! and the check is `cargo tree -p aterm -e normal --target <t> -i <name>`
//! showing OUR path — a package count alone would have looked perfect.
//!
//! CORRECTION, from an adversarial review that reproduced the inert tree rather
//! than reading this note: `cargo forge check` EXITS 1 on it, with six
//! `✗ FAIL` findings. This repository's own patch-liveness obligation [OB-12]
//! already catches the case. An earlier draft here said "nothing warned", which
//! was wrong and would have argued for building a gate that already exists. What
//! is genuinely blind is cargo itself, and any check that counts PACKAGES —
//! the substitution is 1-for-1 whichever copy wins.
//!
//! The round before this one (2026-08-28) retired `base64`, `flate2` +
//! `miniz_oxide` (for `crates/aterm-codec`), `serde_json` + `zmij` (for
//! `crates/aterm-json`) and `memchr` (for `crates/aterm-search`).
//!
//! In THAT round, ten packages left mac-arm, win and wasm: `base64`, `flate2`, `miniz_oxide`,
//! `crc32fast`, `adler2`, `simd-adler32`, `serde_json`, `zmij`, `itoa` and
//! `memchr`. 67,932 lines per cell, identical to the line in all three, because
//! every one of them is target-independent source.
//!
//! LINUX LOST ONLY NINE, and the ninth is the instructive one. `memchr 2.8.1`
//! is still in that graph, held by
//! `quick-xml -> wayland-scanner -> smithay-client-toolkit -> sctk-adwaita ->
//! winit`, which is the Wayland backend. Retiring aterm's edge to it did not
//! remove the package there; it removed aterm's CLAIM on it — the same shape
//! `rustix` had on this cell one round ago, and the reason linux fell 52,133
//! lines rather than 67,932.
//!
//! `workspace` rose by ONE in every cell: `crates/aterm-json`. The other three
//! retirements landed in crates that already existed — `aterm-codec` took
//! base64 and the inflate stream, `aterm-search` took the scanners.
//!
//! No dominator anchor moved. `MAC_ARM_DOMINATORS`, `LINUX_DOMINATORS` and
//! `MAC_ARM_DUPLICATE_NAMES` all re-measure exactly as pinned, which is what a
//! retirement of leaf-ish utility packages should look like: none of these ten
//! was the sole parent of anything a supported root did not already hold.
//!
//! Before those (2026-08-28) came `png` (for `crates/aterm-png`)
//! and `rustix` (for `crates/aterm-dirfd`), and moved `security-framework` from
//! a direct dependency to a dev-only oracle. `security-framework` is the case
//! worth reading twice: its retirement as a DIRECT dependency moved no cell
//! total at all — the package was still in the graph, reached by
//! `ureq -> rustls-platform-verifier -> security-framework` — but it moved a
//! pinned dominator, because `ureq` became those packages' sole parent and
//! absorbed their 3 packages / 16,552 LOC. A dependency that costs the totals
//! nothing can still cost a dominator; that is exactly what dominators are for,
//! and the survey totals alone would never have shown it.
//!
//! Before that (2026-08-27) came `toml` + `toml_edit` for `crates/aterm-toml` —
//! six packages, 64,783 lines per cell — and before that (2026-08-25)
//! `pollster`, `ab_glyph_rasterizer`, `rand_core`, `web-time`, `tar` (with
//! `xattr` and `filetime`) and `font8x8`.

/// One cell's measured surface, in the order [`crate::resolve::default_cells`]
/// reports them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Baseline {
    /// The cell handle, so a row cannot silently drift onto another cell.
    pub cell: &'static str,
    /// Every package in the graph rooted at the shipped `aterm` binary.
    pub resolved: usize,
    /// Workspace members among them — `resolved - third_party`.
    ///
    /// This moves too, and not only when a third-party package leaves: 53 → 54
    /// across every cell when `aterm-time` was created to retire `web-time`. A
    /// retirement that lands as a new first-party crate ADDS a workspace member
    /// while removing a third-party one, so `resolved` falls by less than
    /// `third_party` does.
    pub workspace: usize,
    /// Packages aterm does not own. THE number this crate exists to shrink.
    pub third_party: usize,
    /// Physical `*.rs` lines over those packages (`rs-physical-all-files-v1`).
    pub third_party_loc: u64,
    /// Third-party build scripts: arbitrary code the compiler runs, each one
    /// marked `-Ztrust-verify=off` unconditionally by `targo trust`.
    pub build_scripts: usize,
    /// Third-party proc macros: code compiled and EXECUTED inside rustc.
    pub proc_macros: usize,
    /// Names resolved at two or more versions — the dedup opportunity, which is
    /// not the same prize as a removal.
    pub duplicate_names: usize,
}

pub const MAC_ARM: Baseline = Baseline {
    cell: "mac-arm",
    resolved: 167,
    workspace: 66,
    third_party: 101,
    third_party_loc: 1_487_430,
    build_scripts: 21,
    proc_macros: 6,
    duplicate_names: 8,
};

pub const LINUX: Baseline = Baseline {
    cell: "linux",
    resolved: 272,
    workspace: 67,
    third_party: 205,
    third_party_loc: 3_366_041,
    build_scripts: 36,
    proc_macros: 16,
    duplicate_names: 8,
};

pub const WIN: Baseline = Baseline {
    cell: "win",
    resolved: 172,
    workspace: 65,
    third_party: 107,
    third_party_loc: 3_991_411,
    build_scripts: 23,
    proc_macros: 7,
    duplicate_names: 4,
};

pub const WASM: Baseline = Baseline {
    cell: "wasm",
    resolved: 159,
    workspace: 65,
    third_party: 94,
    third_party_loc: 1_404_079,
    build_scripts: 22,
    proc_macros: 7,
    duplicate_names: 3,
};

/// The four cells, in [`crate::resolve::default_cells`] order, so a test that
/// already holds a cell index can index this too.
pub const CELLS: [Baseline; 4] = [MAC_ARM, LINUX, WIN, WASM];

/// The names duplicated in the mac-arm cell. Pinned as NAMES rather than a
/// count because which crate is doubled is the actionable half of the fact.
pub const MAC_ARM_DUPLICATE_NAMES: [&str; MAC_ARM.duplicate_names] = [
    "bitflags",
    "block2",
    "core-foundation",
    "foldhash",
    "hashbrown",
    "objc2",
    "objc2-foundation",
    "ttf-parser",
];

/// `hashbrown` is the worst of them: three live versions in one binary.
pub const MAC_ARM_HASHBROWN_VERSIONS: usize = 3;

// --------------------------------------------------------- dominator anchors

/// One measured `dom(C) = reach(root) \ reach(root, block C)`.
///
/// These are the regression teeth: a dominator is the only honest answer to
/// "what does this dependency cost", and it moves for reasons a package count
/// alone never shows — see [`MAC_ARM_DOMINATORS`] on `wgpu`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dom {
    pub name: &'static str,
    /// The version, when the NAME alone does not identify one package in the
    /// cell. `None` asserts uniqueness — the test fails loudly if a name it
    /// was given without a version later resolves twice, which is a real
    /// change worth seeing. `Some` is required for a duplicated name: eight of
    /// them exist on mac-arm ([`MAC_ARM_DUPLICATE_NAMES`]), and
    /// `objc2-foundation` is one of the five anchors.
    pub version: Option<&'static str>,
    /// Packages removed, INCLUDING the target. A leaf costs 1, never 0.
    pub pkgs: usize,
    /// Physical `*.rs` lines summed over those packages.
    pub loc: u64,
}

/// The mac-arm anchors, in dominator-LOC order — which makes this list exactly
/// the head of `dominator::ranked`, and the ranking test asserts that.
///
/// `wgpu` GREW here TWICE, for the same reason both times — a package two
/// parents held in is billed to neither, and retiring one parent leaves the
/// other holding it alone.
///
/// 29 packages / 398,302 LOC → 31 / 424,106 when the `a11y-accesskit` default
/// was dropped: `accesskit_consumer` and `accesskit_macos` also depended on
/// `hashbrown 0.16.1`, so it and `foldhash 0.2.0` were shared. Then 31 / 424,106
/// → 33 / 464,874 when `toml_edit` left with `toml`. MEASURED, exactly:
/// `blame indexmap --cell mac-arm` reports `dom 2 package(s) / 40,768 LOC` and
/// its direct dependants went from `naga, toml_edit, wgpu-core` to `naga,
/// wgpu-core` — both inside wgpu's subtree — so wgpu absorbed indexmap's whole
/// dominator, to the line. An extraction that shrinks the surface can enlarge a
/// dominator; that is the definition working, not a regression.
///
/// `ureq` is the third instance of the same shape, and the first to change this
/// list's ORDER. It entered at rank four, 9 packages / 72,528 LOC → 12 / 89,080,
/// when `security-framework` stopped being one of aterm-gui's direct
/// dependencies (0f92b2f1, Keychain over `SecItem*` FFI). That retirement moved
/// no cell TOTAL — the package never left the graph — but it left
/// `ureq -> rustls-platform-verifier` as the sole parent of
/// `security-framework 3.7.0` (10,503), `security-framework-sys 2.17.0` (2,213)
/// and `core-foundation 0.10.1` (3,836), and ureq absorbed all three: 16,552
/// LOC, to the line. It displaced `tracing` by 4,597 LOC, and pushed
/// `objc2-app-kit` (1 / 82,976) off the head entirely.
///
/// So a dependency can be retired, leave the totals untouched, and still be
/// exactly what this file exists to notice. Nothing but a dominator would have
/// reported it.
///
/// `tracing` is GONE from this list, and it is the first anchor retired by
/// being REPLACED rather than removed. Its dominator was 3 packages / 84,483
/// LOC — `tracing` itself, `tracing-core` and `tracing-attributes` — and every
/// event through all of it dispatched to `NoSubscriber`, because aterm installs
/// no subscriber and `tracing-subscriber` is in no cell's graph. A 1,541-line
/// first-party facade (`crates/aterm-tracing`, package `tracing 0.1.44`,
/// patched in so winit, softbuffer, zbus and tiny-xlib all reach it) does the
/// same nothing, and the whole 84,483 came off the mac-arm row: 1,582,450 →
/// 1,497,967, exactly the dominator, with 108 → 105 packages. The tail falls to
/// `objc2-foundation 0.3.2`.
///
/// ONE MEASUREMENT TRAP is worth recording, because it silently under-reported
/// this by a factor of seven: `loc::package_dir` used to resolve
/// `<name>-<version>` against the registry checkout BEFORE the workspace, which
/// is right for a vendored fork (the pristine copy is the surface) and wrong
/// for a first-party replacement — it measured our 1,541-line crate as
/// upstream's 72,271-line one, and the win read as 12,212 lines. Workspace
/// members now win, by manifest name.
pub const MAC_ARM_DOMINATORS: [Dom; 5] = [
    Dom {
        name: "wgpu",
        version: None,
        pkgs: 31,
        loc: 460_851,
    },
    Dom {
        name: "naga",
        version: None,
        pkgs: 8,
        loc: 212_320,
    },
    Dom {
        name: "libc",
        version: None,
        pkgs: 1,
        loc: 127_772,
    },
    // See the note above: these are the entries that moved, and they moved
    // because a dependency was retired somewhere else entirely.
    // `regex` stood at the tail here at 4 packages / 158,471 lines until
    // crates/aterm-regex retired it, then `objc2-app-kit` (1 / 82,976) held the
    // slot, then `ureq` (12 / 89,080) and `tracing` (3 / 84,483) took the
    // fourth in turn. The array does not shrink when the campaign succeeds — an
    // anchor list that thins out with every win stops being a regression net —
    // so the slot goes to whatever now ranks by LOC. The list is ORDERED, so a
    // wrong pick here fails the ranking test rather than passing quietly.
    Dom {
        name: "objc2-app-kit",
        version: None,
        pkgs: 1,
        loc: 82_976,
    },
    // The tail after `tracing` was replaced. Worth noting what these five are:
    // a GPU abstraction, a shader compiler, the platform ABI, and two ObjC
    // binding surfaces — every one a Lane 1 crate. The extraction lane has run
    // out of things it can honestly reach, which is the campaign's own progress
    // showing up in its regression net. (`objc2-foundation` is one of the eight
    // duplicated names on this cell; this is the 0.3.2 copy, and the 0.2.2 one
    // ranks separately at 2 / 60,733.)
    Dom {
        name: "objc2-foundation",
        version: Some("0.3.2"),
        pkgs: 1,
        loc: 78_448,
    },
];

/// The linux anchor.
///
/// The two that used to sit beside it — `accesskit_unix` (57 packages /
/// 241,084 LOC) and `accesskit_winit` (58 / 242,598) — are GONE from the graph,
/// not merely cheaper: dropping the `a11y-accesskit` default removed them and
/// the 61 packages they alone held in, which is why the linux cell fell from
/// 301 resolved / 248 third-party to the row in [`LINUX`].
///
/// `sctk-adwaita` GREW here, from 7 packages / 31,776 LOC, when
/// `crates/aterm-render` stopped depending on `ab_glyph_rasterizer`. The
/// package did not leave the linux graph — `winit -> sctk-adwaita -> ab_glyph`
/// still holds it, which is why linux kept it while mac, win and wasm all shed
/// it — but it stopped being SHARED, and a package two parents hold in is
/// billed to neither. Now sctk-adwaita is the only thing keeping it, and the
/// dominator says so. This is the second time in two rounds that a successful
/// extraction enlarged a dominator (see [`MAC_ARM_DOMINATORS`] on `wgpu`); it is
/// the measure working, not drift.
pub const LINUX_DOMINATORS: [Dom; 1] = [Dom {
    name: "sctk-adwaita",
    version: None,
    pkgs: 8,
    loc: 32_341,
}];

/// `ureq` on mac-arm, and the figure the design note recorded for it.
///
/// The design note says 8 packages / 71,834 LOC; this checkout measures FOUR
/// packages and 17,246 lines more, and every one of them is accounted for:
///
/// * `percent-encoding 2.3.2` (694 LOC), whose only parent in the mac-arm graph
///   has always been `ureq` itself, so it must fall with ureq. That was the
///   whole difference until 2026-08-28.
/// * `security-framework 3.7.0` (10,503), `security-framework-sys 2.17.0`
///   (2,213) and `core-foundation 0.10.1` (3,836) — 16,552 LOC — which ureq
///   ABSORBED when aterm-gui stopped depending on `security-framework`
///   directly. They did not join the graph; they stopped being shared, which
///   bills them to their one remaining parent. The chain is
///   `ureq -> rustls-platform-verifier -> security-framework`, and it exists
///   because crates/aterm-gui/Cargo.toml asks ureq for `platform-verifier`
///   deliberately (system trust roots, not a bundled bundle).
///
/// Both halves are pinned, and [`UREQ_RE_PARENTED`] names the three by hand, so
/// the next person to re-measure sees the REASON rather than a number that
/// moved.
pub const MAC_ARM_UREQ: Dom = Dom {
    name: "ureq",
    version: None,
    pkgs: 12,
    loc: 89_080,
};
/// The three packages `ureq` absorbed when aterm-gui's direct
/// `security-framework` edge went away, with their measured LOC. Subtracting
/// these and `percent-encoding` from [`MAC_ARM_UREQ`] reproduces
/// [`UREQ_DESIGN_NOTE`] exactly, which is what makes the delta bookkeeping
/// rather than a different graph.
/// The VERSION is part of each entry because `core-foundation` resolves twice
/// on mac-arm — 0.10.1 under ureq and 0.9.4 under winit — and only the first is
/// ureq's to pay for.
pub const UREQ_RE_PARENTED: [(&str, &str, u64); 3] = [
    ("security-framework", "3.7.0", 10_503),
    ("security-framework-sys", "2.17.0", 2_213),
    ("core-foundation", "0.10.1", 3_836),
];
/// The design-note figure `MAC_ARM_UREQ` exceeds by `percent-encoding` and the
/// three names in [`UREQ_RE_PARENTED`].
pub const UREQ_DESIGN_NOTE: Dom = Dom {
    name: "ureq",
    version: None,
    pkgs: 8,
    loc: 71_834,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::default_cells;

    /// The table is indexed by cell position elsewhere, so the positions must
    /// agree with the cell matrix or a test would pin the wrong cell's numbers.
    #[test]
    fn the_baseline_rows_line_up_with_the_cell_matrix() {
        let cells = default_cells();
        assert_eq!(cells.len(), CELLS.len());
        for (cell, base) in cells.iter().zip(CELLS) {
            assert_eq!(cell.name, base.cell, "baseline row out of order");
        }
    }

    /// Internal arithmetic, checked here so no per-cell test has to restate it.
    #[test]
    fn every_row_partitions_its_graph_into_workspace_and_third_party() {
        for base in CELLS {
            assert_eq!(
                base.workspace + base.third_party,
                base.resolved,
                "cell `{}` does not partition",
                base.cell
            );
            assert!(base.third_party_loc > 0 && base.proc_macros > 0);
        }
    }
}
