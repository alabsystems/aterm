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
//! Last measured: 2026-08-25, on the tree that retired the SIX-package round:
//! `pollster`, `ab_glyph_rasterizer`, `rand_core`, `web-time`, `tar` (with
//! `xattr` and `filetime`) and `font8x8`. Every cell fell, and `workspace` rose
//! by one because that round created `aterm-time`.
//!
//! Two things in this file moved for a reason worth reading before assuming
//! drift: `LINUX_DOMINATORS`' `sctk-adwaita` GREW, and `MAC_ARM.workspace` went
//! from 53 to 54. Both are written up where they are pinned.

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
    resolved: 191,
    workspace: 55,
    third_party: 136,
    third_party_loc: 1_871_883,
    build_scripts: 26,
    proc_macros: 6,
    duplicate_names: 8,
};

pub const LINUX: Baseline = Baseline {
    cell: "linux",
    resolved: 292,
    workspace: 55,
    third_party: 237,
    third_party_loc: 3_662_288,
    build_scripts: 40,
    proc_macros: 17,
    duplicate_names: 12,
};

pub const WIN: Baseline = Baseline {
    cell: "win",
    resolved: 196,
    workspace: 55,
    third_party: 141,
    third_party_loc: 4_305_179,
    build_scripts: 27,
    proc_macros: 7,
    duplicate_names: 5,
};

pub const WASM: Baseline = Baseline {
    cell: "wasm",
    resolved: 182,
    workspace: 55,
    third_party: 127,
    third_party_loc: 1_712_200,
    build_scripts: 26,
    proc_macros: 7,
    duplicate_names: 4,
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
    /// Packages removed, INCLUDING the target. A leaf costs 1, never 0.
    pub pkgs: usize,
    /// Physical `*.rs` lines summed over those packages.
    pub loc: u64,
}

/// The mac-arm anchors, in dominator-LOC order — which makes this list exactly
/// the head of `dominator::ranked`, and the ranking test asserts that.
///
/// `wgpu` GREW here, from 29 packages / 398,302 LOC, when the `a11y-accesskit`
/// default feature was dropped. Nothing about wgpu changed: `accesskit_consumer`
/// and `accesskit_macos` also depended on `hashbrown 0.16.1`, so that package
/// (with `foldhash 0.2.0`, 2 packages / 25,804 LOC between them) was shared and
/// therefore billed to neither. With accesskit out of the graph, wgpu is the
/// only thing holding hashbrown in, and the dominator says so. An extraction
/// that shrinks the surface can enlarge a dominator; that is the definition
/// working, not a regression.
pub const MAC_ARM_DOMINATORS: [Dom; 5] = [
    Dom {
        name: "wgpu",
        pkgs: 31,
        loc: 424_106,
    },
    Dom {
        name: "naga",
        pkgs: 8,
        loc: 212_320,
    },
    Dom {
        name: "libc",
        pkgs: 1,
        loc: 127_772,
    },
    Dom {
        name: "tracing",
        pkgs: 3,
        loc: 84_483,
    },
    // `regex` stood here at 4 packages / 158,471 lines until crates/aterm-regex
    // retired it. `rustix` takes the slot rather than shrinking the array: an
    // anchor list that thins out every time the campaign succeeds stops being a
    // regression net. The slot goes to whatever now ranks fifth by LOC, which is
    // `objc2-app-kit` (82,976) — NOT `rustix` (72,832); the list is ordered, so a
    // wrong pick here fails the ranking test rather than passing quietly.
    Dom {
        name: "objc2-app-kit",
        pkgs: 1,
        loc: 82_976,
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
    pkgs: 8,
    loc: 32_341,
}];

/// `ureq` on mac-arm, and the figure the design note recorded for it.
///
/// The design note says 8 packages / 71,834 LOC; this checkout measures one
/// package more. The whole difference is `percent-encoding 2.3.2` (694 LOC),
/// whose only parent in the mac-arm graph is `ureq` itself — so it must fall
/// with ureq. Both halves are pinned so the next person to re-measure sees the
/// REASON, not just a number that moved.
pub const MAC_ARM_UREQ: Dom = Dom {
    name: "ureq",
    pkgs: 9,
    loc: 72_528,
};
/// The design-note figure `MAC_ARM_UREQ` exceeds by `percent-encoding` alone.
pub const UREQ_DESIGN_NOTE: Dom = Dom {
    name: "ureq",
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
