// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The measured baseline: one place, read by every pinned test.
//!
//! Every number is a measurement of this checkout by the code the tests
//! exercise (`targo --unverified forge survey`, cross-checked against
//! `tools/forge-budget.tsv`). The cell rows are ceilings in `loc`'s tests, so
//! unplanned graph growth fails a test. Keeping them in one file makes updating
//! a baseline one edit.
//!
//! # Relation to the ratchet
//!
//! `tools/forge-budget.tsv` is the ratchet, gated manually by `targo
//! --unverified forge budget`. This module is the ceiling `targo --unverified test -p
//! aterm-forge` holds automatically. `ratchet_agreement` ties them: every cell
//! has all five ratchet rows, no TSV ceiling sits above its row here, and
//! `loc`'s cell tests hold the live graph under both, so growing the surface
//! needs a TSV edit. A retirement leaves this file alone; `forge budget
//! --update` lowers the TSV.
//!
//! # Re-measuring
//!
//! ```text
//! targo --unverified forge survey                       # the per-cell table
//! targo --unverified forge blame <name> --cell <cell>   # one dominator
//! ```
//!
//! Copy what the tool prints; the reason for a change belongs in the commit
//! message.
//!
//! The wasm cells are the two shipped browser modules, [`WASM_CPU`]
//! (`crates/aterm-wasm`) and [`WASM_GPU`] (`crates/aterm-gpu-web`); see
//! [`crate::resolve::default_cells`].

/// One cell's measured surface, in the order [`crate::resolve::default_cells`]
/// reports them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Baseline {
    /// The cell handle, so a row cannot silently drift onto another cell.
    pub cell: &'static str,
    /// Every package in the graph rooted at the shipped `aterm` binary.
    pub resolved: usize,
    /// Workspace members among them: `resolved - third_party`. A retirement
    /// that lands as a new first-party crate raises it.
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

// The winit rows measure the vendored fork as it compiles, including its local
// patches (attest.rs counts the markers), among them:
//   - monitor.rs: no `expect` on a display that vanished (LOCAL PATCH)
//   - window_delegate.rs: no `unwrap` on a gone screen in full screen (LOCAL PATCH)
//   - raw sends through aterm_objc (RcBlock, fullscreen state) (LOCAL PATCH)
//   - swizzle and `MainThreadBound` ports replacing objc2 (LOCAL PATCH)
//   - app_state.rs: waker stopped inside a nested run loop (LOCAL PATCH)
//   - observer.rs: the waker is `aterm_objc::WakeTimer` (LOCAL PATCH)
//   - macos/headless.rs and event_loop.rs: the display-free `--headless` loop,
//     no NSApplication (LOCAL PATCH)
pub const MAC_ARM: Baseline = Baseline {
    cell: "mac-arm",
    resolved: 124,
    workspace: 77,
    third_party: 47,
    third_party_loc: 442_113,
    build_scripts: 10,
    proc_macros: 2,
    duplicate_names: 1,
};

pub const LINUX: Baseline = Baseline {
    cell: "linux",
    resolved: 274,
    workspace: 79,
    third_party: 195,
    third_party_loc: 2_790_322,
    build_scripts: 32,
    proc_macros: 16,
    duplicate_names: 6,
};

pub const WIN: Baseline = Baseline {
    cell: "win",
    resolved: 161,
    workspace: 73,
    third_party: 88,
    third_party_loc: 3_204_104,
    build_scripts: 18,
    proc_macros: 7,
    duplicate_names: 0,
};

/// The CPU browser module, `crates/aterm-wasm`: the engine plus the
/// `aterm-render` rasterizer, blitted with `putImageData`.
pub const WASM_CPU: Baseline = Baseline {
    cell: "wasm-cpu",
    resolved: 62,
    workspace: 40,
    third_party: 22,
    third_party_loc: 221_029,
    build_scripts: 6,
    proc_macros: 2,
    duplicate_names: 0,
};

/// The GPU browser module, `crates/aterm-gpu-web`: the same engine plus
/// `aterm-gpu` over `wgpu`'s WebGL2 backend. The gap to [`WASM_CPU`] is
/// `wgpu`'s subtree; its dominator (`blame wgpu --cell wasm-gpu`) is smaller,
/// since `web-sys`, `raw-window-handle` and `wasm-bindgen-futures` have direct
/// edges from `aterm-gpu-web`.
pub const WASM_GPU: Baseline = Baseline {
    cell: "wasm-gpu",
    resolved: 103,
    workspace: 43,
    third_party: 60,
    third_party_loc: 952_607,
    build_scripts: 16,
    proc_macros: 6,
    duplicate_names: 0,
};

// ---------------------------------------------------------------------------
// The second architecture of each shipped OS. Per-target resolution keys
// mostly on `target_os`/`target_family`, so these usually equal their
// siblings; the rows catch an arch-gated dependency. `targo --unverified forge
// survey --cell mac-x64 --cell linux-arm --cell win-arm` prints all three.
// ---------------------------------------------------------------------------

/// The Intel-Mac slice of the universal release binary (`aterm-release`'s
/// buildplan.rs builds it under upstream stable). Identical to [`MAC_ARM`] in
/// every field.
pub const MAC_X64: Baseline = Baseline {
    cell: "mac-x64",
    resolved: 124,
    workspace: 77,
    third_party: 47,
    third_party_loc: 442_113,
    build_scripts: 10,
    proc_macros: 2,
    duplicate_names: 1,
};

/// ARM Linux — the triple tools/linux-auto-atpkg.sh cross-packs beside x86_64.
/// Identical to [`LINUX`] in every field.
pub const LINUX_ARM: Baseline = Baseline {
    cell: "linux-arm",
    resolved: 274,
    workspace: 79,
    third_party: 195,
    third_party_loc: 2_790_322,
    build_scripts: 32,
    proc_macros: 16,
    duplicate_names: 6,
};

/// ARM Windows: the triple apps/aterm-win/build.ps1 selects on an ARM64 host.
/// Differs from [`WIN`] by windows-sys 0.52, which ring 0.17's aarch64
/// CPU-feature probe still resolves here.
pub const WIN_ARM: Baseline = Baseline {
    cell: "win-arm",
    resolved: 164,
    workspace: 73,
    third_party: 91,
    third_party_loc: 3_588_682,
    build_scripts: 19,
    proc_macros: 7,
    duplicate_names: 1,
};

/// The eight cells, in [`crate::resolve::default_cells`] order, so a test
/// holding a cell index can index this too. Append-only.
pub const CELLS: [Baseline; 8] = [
    MAC_ARM, LINUX, WIN, WASM_CPU, WASM_GPU, MAC_X64, LINUX_ARM, WIN_ARM,
];

/// The names duplicated in the mac-arm cell, pinned as names because which
/// crate is doubled is the actionable fact. `bitflags` is 1.3.2 under
/// core-graphics beside 2.x elsewhere.
pub const MAC_ARM_DUPLICATE_NAMES: [&str; MAC_ARM.duplicate_names] = ["bitflags"];

/// `hashbrown`'s resolved version count on mac-arm (none of its parents are in
/// that graph). Counted over resolved versions rather than looked up in the
/// duplicate map, which has no key for a single-version name.
pub const MAC_ARM_HASHBROWN_VERSIONS: usize = 0;

/// The tie between this file and `tools/forge-budget.tsv`, read from the TSV
/// alone (no `cargo tree`).
///
/// Deleting a scope's rows from the TSV would otherwise leave the gate green
/// (they fall to an advisory "UNRATCHETED" list), and uncompared metrics could
/// be raised without `--allow-regress`'s reason rule. The relation is
/// `TSV ceiling <= measured row`; a const raised here with the TSV unchanged is
/// caught by `loc`'s cell tests.
#[cfg(test)]
pub(crate) mod ratchet_agreement {
    use super::{Baseline, CELLS};
    use crate::budget::{self, Row};

    /// The ratchet's scope string for each [`CELLS`] row, in order. The browser
    /// modules append their handle (`budget::scope_of`). Spelled out so a
    /// change to either side is a diff.
    const SCOPES: [(&str, &str); 8] = [
        ("mac-arm", "shipped.aarch64-apple-darwin"),
        ("linux", "shipped.x86_64-unknown-linux-gnu"),
        ("win", "shipped.x86_64-pc-windows-msvc"),
        ("wasm-cpu", "shipped.wasm32-unknown-unknown.wasm-cpu"),
        ("wasm-gpu", "shipped.wasm32-unknown-unknown.wasm-gpu"),
        ("mac-x64", "shipped.x86_64-apple-darwin"),
        ("linux-arm", "shipped.aarch64-unknown-linux-gnu"),
        ("win-arm", "shipped.aarch64-pc-windows-msvc"),
    ];

    /// The five per-cell metrics the TSV ratchets, in its spelling, with
    /// `base`'s figure for each. (`packages` is the sixth shipped metric and
    /// is deliberately unseeded there — `budget`'s module doc says why.)
    pub(crate) fn ratcheted(base: &Baseline) -> [(&'static str, u64); 5] {
        [
            ("third_party_packages", base.third_party as u64),
            ("third_party_loc", base.third_party_loc),
            ("build_scripts", base.build_scripts as u64),
            ("proc_macros", base.proc_macros as u64),
            ("duplicate_names", base.duplicate_names as u64),
        ]
    }

    /// The real `tools/forge-budget.tsv`. An absent or empty file is NOT
    /// skipped: it is every row missing at once.
    pub(crate) fn load() -> Vec<Row> {
        budget::load(&crate::test_support::repo_root())
            .unwrap_or_else(|e| panic!("{}: {e}", budget::BUDGET_PATH))
    }

    /// The TSV ceiling for each of [`ratcheted`]'s metrics in cell `index`,
    /// in the same order. A missing row PANICS, by scope and metric.
    pub(crate) fn tsv_ceilings(rows: &[Row], index: usize) -> [(&'static str, u64); 5] {
        let (cell, scope) = SCOPES[index];
        assert_eq!(CELLS[index].cell, cell, "CELLS order changed");
        ratcheted(&CELLS[index]).map(|(metric, _)| {
            let ceiling = rows
                .iter()
                .find(|r| r.scope == scope && r.metric == metric)
                .map(|r| r.ceiling)
                .unwrap_or_else(|| {
                    panic!(
                        "{cell}: {} has NO `{metric}` row for scope `{scope}`. A measured \
                         cell with no ratchet row is a SILENT GREEN — the gate lists its \
                         figure as advisory and moves on — and `--update` cannot add rows, \
                         so nothing but this assertion holds the scope in the file. Write \
                         the row.",
                        budget::BUDGET_PATH
                    )
                });
            (metric, ceiling)
        })
    }

    #[test]
    fn every_cell_has_all_five_ratchet_rows_and_none_above_its_measured_row() {
        let rows = load();
        for (index, base) in CELLS.iter().enumerate() {
            for ((metric, ceiling), (_, measured)) in
                tsv_ceilings(&rows, index).into_iter().zip(ratcheted(base))
            {
                assert!(
                    ceiling <= measured,
                    "{}: {} ceiling for `{metric}` is {ceiling}, above measured::{}'s \
                     {measured}. A TSV row raised alone skips `--allow-regress`'s reason \
                     rule; a deliberate raise re-measures the row here in the same change.",
                    base.cell,
                    budget::BUDGET_PATH,
                    base.cell.to_uppercase().replace('-', "_"),
                );
            }
        }
    }
}
