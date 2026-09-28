// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE measured baseline — one place, read by every pinned test.
//!
//! # Why this module exists
//!
//! Every number below is a real measurement of this checkout, taken by the very
//! code the tests exercise (`cargo forge survey`, cross-checked against
//! `tools/forge-budget.tsv`). The cell rows are CEILINGS in `loc`'s tests: a
//! graph that grows without anyone deciding it should is exactly the failure
//! this crate was built to catch, and an assertion is the only thing that
//! notices.
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
//! ever decreases, and `cargo forge budget` is the gate on it — a manual one
//! (`gate forge` is outside the `verify --fast` ladder). This module is the
//! ceiling `cargo test -p aterm-forge` holds automatically, and the two are
//! TIED by `ratchet_agreement` (below): every cell must have all five ratchet rows
//! (a missing row is only an advisory UNRATCHETED line to the gate), no TSV
//! ceiling may sit above its row here, and `loc`'s cell tests hold the live
//! graph under BOTH files — so growing the surface takes an edit to the TSV,
//! not just to a const in this file.
//!
//! These rows were EQUALITY pins until 2026-09-24, catching motion in either
//! direction; the downward half made every retirement a red suite until the
//! rows were re-copied, so it was dropped. A retirement now leaves this file
//! alone, `cargo forge budget --update` lowers the TSV, and everything stays
//! green. The dominator anchors below are RECORDS of what was measured, not
//! asserted by any test: re-measure one with `cargo forge blame`.
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
//! # THE wasm ROWS WERE NOT A MEASUREMENT OF ANYTHING SHIPPED, until 2026-08-30
//!
//! Every `wasm` figure in the notes below — and the `WASM` baseline they
//! justified, 81 packages / 1,172,582 lines — came from a cell rooted at the
//! package `aterm`. `aterm` is a `[[bin]]`; nothing compiles it for
//! `wasm32-unknown-unknown`. `cargo tree` resolves it for that triple regardless,
//! so the row was a real measurement of a configuration that is never built, and
//! it was wrong in BOTH directions: it counted `zstd` (a C library that cannot
//! target wasm32, and which both web crates switch off), `winit`, `rustls` and
//! the whole updater, and it MISSED `console_error_panic_hook`, which both
//! shipped modules declare and the `aterm` root never reaches.
//!
//! It is two rows now, one per artifact aterm actually ships to a browser:
//! [`WASM_CPU`] (`crates/aterm-wasm`) at 27 / 255,826 and [`WASM_GPU`]
//! (`crates/aterm-gpu-web`) at 64 / 984,913. Those are the two crates the only
//! two lanes that build wasm at all — `xtask gate web` and
//! `tools/wasm-bench/run.sh` — name explicitly.
//!
//! THIS IS A RESTATED DENOMINATOR, NOT A RETIREMENT. Nothing left the graph on
//! 2026-08-30; the old number measured a target that does not exist. No wasm
//! figure in the history below is comparable to either row above, and they are
//! left as written because they are the record of what was measured then.
//! [`crate::resolve::default_cells`] carries the full derivation.
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

// ---------------------------------------------------------------------------
// RE-MEASURED 2026-08-30 — a MEASUREMENT change, and ONLY the LOC rows move.
//
// `loc::package_dir` now resolves a patched package to the path that COMPILES,
// before the registry is consulted. It used to prefer a pristine registry
// checkout of the same version and fall through to `vendor/<name>` when the
// machine had none, SILENTLY — so these pins recorded whichever the measuring
// box's CARGO_HOME happened to hold, and the same commit read green on the
// ratcheting machine and red on every other. Cargo cannot fetch a pristine copy
// for a patched package at all (source-less lock entry), so that branch could
// never have been relied on.
//
// The evidence that this is not dependency drift is in the shape of the diff:
// `resolved`, `workspace`, `third_party`, `build_scripts`, `proc_macros` and
// `duplicate_names` are UNCHANGED in every one of the five cells. Only
// `third_party_loc` moves, by each cell's own forks' edits over upstream
// (+811 on mac-arm, linux and win; +15 wasm-cpu; +98 wasm-gpu), and in
// `MAC_ARM_DOMINATORS` only `wgpu` (+83) and `winit` (+713) — winit being the
// fork itself. Recorded in `tools/forge-budget.tsv` through the tool's own
// `--allow-regress` channel, with that reason.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// RE-MEASURED 2026-08-31 — THE FLIP (map §5 W6, delivered).
//
// wgpu left the macOS normal dependency graph: the first-party Metal renderer
// is the default and only macOS arm, and wgpu survives on the cell solely as
// the differential ORACLE, activated by aterm-gpu's target-gated self-dev-
// dependency (`wgpu-oracle`) — a dev edge, invisible to `cargo tree -e
// normal` and therefore to every number in this file. `blame wgpu --cell
// mac-arm` now answers NOT RESOLVED, and the mac-arm row collapsed by exactly
// the pinned prize: 88 -> 51 third-party packages (-37 = dom(wgpu).pkgs) and
// 1,224,481 -> 589,449 lines against the +652-drifted pre-flip live (the
// difference to the collapse below is the winit-fork edits recorded in the
// ratchet's --allow-regress reason; the dom itself came off at 635,044 to the
// line). The resolved count also sheds the two vendored wgpu shims
// (wgpu-naga-bridge, wgpu-core-deps-apple): -39 nodes total, 0 added.
// linux/win/wasm-gpu/wasm-cpu package sets AND feature sets diffed
// byte-identical to pre-flip; their loc rows carry only the pre-existing
// winit-fork drift (+652 owner headless arm, +12 §4(b) notices attest was
// owed — see the ratchet reason).
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// RE-MEASURED 2026-09-01 — the post-flip sweep, item 1 of 2: `bytemuck_derive`.
//
// A PROC MACRO left the mac-arm cell, and it is the flip that made it
// purchasable. `bytemuck`'s `derive` feature had two requesters on this cell
// before — `wgpu-types` and `wgpu-hal` both name `bytemuck/derive` in their own
// manifests — so aterm-gpu dropping the feature bought exactly nothing while
// wgpu was in the normal graph. Post-flip aterm-gpu was the SOLE activator
// (rustybuzz, the row's other parent, asks only for `extern_crate_alloc`), so
// swapping its ten `#[derive(Pod, Zeroable)]` uniform/instance structs onto
// `aterm-bits` — this workspace's own `Pod`/`Zeroable`, already `aterm-core`'s —
// takes the whole package off.
//
//   mac-arm  51 -> 50 third-party, 589,449 -> 586,515 LOC (-2,934, exactly
//            `bytemuck_derive 1.10.2`), 24,898 -> 24,888 unsafe tokens,
//            proc macros 3 -> 2, resolved 117 -> 116. Build scripts, workspace
//            members and the one duplicate name are unchanged.
//
// `bytemuck 1.25.0` ITSELF STAYS, and this is the honest half of the entry: its
// remaining parent is rustybuzz, so the row's other 5,433 lines do not fall
// until the shaper is replaced — at which point they fall for free, which is
// what makes this a pre-payment rather than a whole retirement. No dominator
// anchor moved: `syn`'s parent set drops 4 -> 3 but dom(syn) is 1 package
// either way, because proc-macro2/quote/unicode-ident are reached by
// serde_derive as well.
//
// linux, win, wasm-cpu and wasm-gpu are UNCHANGED to the line: they still
// resolve `bytemuck_derive` through wgpu, which never left those cells.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// RE-MEASURED 2026-09-01 — the post-flip sweep, item 2 of 2: `core_maths`,
// AND WITH IT THE `libm` FORK ON TWO CELLS.
//
// `[patch.crates-io] core_maths = { path = "crates/aterm-core-maths" }`. The
// package itself is 1,221 lines of extension trait; the prize is what it drags
// in — `libm`, which this repository VENDORS (vendor/libm, 19,867 lines and a
// build script). `core_maths` is libm's SOLE parent on mac-arm and on wasm-cpu,
// which is why those two cells lose 2 packages and 21,088 lines while the other
// three lose only core_maths (naga and num-traits keep libm there).
//
//   mac-arm   50 -> 48 third-party, 586,515 -> 565,427 LOC, 24,888 -> 24,831
//             unsafe, build scripts 11 -> 10, resolved 116 -> 115
//   wasm-cpu  25 -> 23, 246,067 -> 224,979, 1,064 -> 1,007 unsafe,
//             build scripts 7 -> 6, resolved 63 -> 62
//   linux     190 -> 189, 2,741,839 -> 2,740,618  (libm STAYS)
//   win        93 ->  92, 3,589,068 -> 3,587,847  (libm STAYS)
//   wasm-gpu   62 ->  61,   957,778 ->   956,557  (libm STAYS)
//
// Every cell also gains ONE workspace member — the replacement crate — which is
// why `resolved` falls by less than `third_party` does. Total across the five:
// 7 third-party packages, 45,839 lines, 2 build scripts.
//
// THE ROW MOVED WITH THE FLIP, and that is the whole reason it was purchasable
// now. Pre-flip, libm had THREE parents on mac-arm (core_maths, naga,
// num-traits), so dom(core_maths) was 1,221 lines — a ~6:1 write and nowhere
// near the taken band. The flip took naga and num-traits off this cell with
// wgpu, leaving core_maths alone over libm and the dominator at 21,088.
//
// WHAT DID NOT MOVE IS ANY EXECUTED INSTRUCTION, and it is measured, not hoped.
// The string `core_maths` occurs exactly four times in the two consumers'
// sources, every one of them a `use core_maths::CoreFloat;` under
// `#[cfg(not(feature = "std"))]`, and `std` is ON for rustybuzz and ttf-parser
// in all five cells. The trait is linked and never imported: rustybuzz's eleven
// `.round()` calls and ttf-parser's `.sin()`/`.cos()`/`.tan()`/`.abs()` already
// resolve to std's inherent methods. `crates/aterm-core-maths/tests/consumers.rs`
// holds both halves of that per cell, and both tripwires were fired once on
// purpose before being restored.
//
// No dominator anchor moved: neither core_maths nor libm is one, and no anchor
// reaches either. `[OB-12]` now records `libm` as live in 3 of 5 cells instead
// of 5 — a NOTE by design ("recorded so a SHRINKING cell set is visible"), not
// a failure.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// RE-MEASURED 2026-09-01 — `once_cell`, the row the post-flip sweep MISSED.
//
// `[patch.crates-io] once_cell = { path = "crates/aterm-once-cell" }`. The
// package is 3,950 lines and 53 unsafe tokens, no build script, no proc macro,
// and — the fact that decides everything else about this row — it is a LEAF.
// It has no dependencies of its own, so dom(once_cell) is 1 package / 3,950
// lines in every cell, and the SAME 1 / 3,950 comes off all five:
//
//   mac-arm   48 -> 47 third-party, 565,427 -> 561,477 LOC, 24,831 -> 24,778 unsafe
//   linux    189 -> 188,            2,740,618 -> 2,736,668
//   win       92 -> 91,             3,587,847 -> 3,583,897
//   wasm-cpu  23 -> 22,               224,979 ->   221,029, 1,007 -> 954 unsafe
//   wasm-gpu  61 -> 60,               956,557 ->   952,607
//
// Build scripts, proc macros, duplicate names and `resolved` are UNCHANGED on
// every cell: the package brought none of the first two, and every cell gains
// one workspace member (the replacement) as it loses one third-party package.
// Total across the five: 5 third-party packages, 19,750 lines, 265 unsafe
// tokens.
//
// THE FLIP DID NOT CREATE THIS ROW, and the correction matters because the
// sweep's judge found the row by looking at post-flip parent sets. A LEAF'S
// DOMINATOR IS ITSELF NO MATTER WHO ITS PARENTS ARE, so dom(once_cell) was 1 /
// 3,950 before W6b too. What the flip changed is only the blame line on
// mac-arm: three parents (rustls, naga, wgpu-core) became one (rustls alone)
// when wgpu left. This row was always buyable and simply was not on the list —
// unlike `core_maths` above, whose dominator genuinely went 1,221 -> 21,088
// because the flip left it alone over the vendored `libm` fork.
//
// A DOMINATOR ANCHOR DID MOVE, and this is the first row in this file where one
// has. `dom(rustls)` on mac-arm falls 6 packages / 69,363 lines -> 5 / 65,413,
// because rustls is once_cell's ONLY parent on that cell, so once_cell sat
// inside rustls's dominator and has now left the third-party graph entirely.
// The mac-arm ranking is unchanged in ORDER (rustls stays third, behind
// objc2-app-kit and winit); only its cost moved. `MAC_ARM_DOMINATORS` is
// updated below rather than the test relaxed.
//
// WHAT MOVED THAT DOES RUN — and this is where `once_cell` stops resembling
// every other first-party patch target. `tracing`, `profiling`, `cfg-if`,
// `log` and `core_maths` are facades, macros, re-exports or `cfg`-ed-off
// imports; NOTHING executed changed when they landed. Here, ten third-party
// crates CALL these types:
//
//   dead   rustls, naga, read-fonts        cfg-gated off in every cell
//   live   ahash                           linux            race::OnceBox
//          wgpu-core                       linux win wasm-gpu   sync::OnceCell
//          x11-dl, xkbcommon-dl            linux            sync::OnceCell
//          wayland-sys, x11rb              linux            sync::Lazy
//          wgpu-hal                        win              sync::Lazy
//          js-sys, wasm-bindgen,
//          wasm-bindgen-futures            wasm             unsync::Lazy
//
// So MAC-ARM is the only cell on which this row is the familiar "linked and
// never called" trade: `rustls`'s import is `#[cfg(not(feature = "std"))]` and
// `std` is on. On the other four the replacement is running code, which is why
// it is the first one here to carry behaviour tests with PLANTED CONTROLS
// (crates/aterm-once-cell/tests/behaviour.rs) instead of a liveness tripwire
// alone. The sharpest of them: wgpu-core's `ResourcePool` relies on
// `get_or_try_init` calling its closure exactly once under contention, and the
// obvious wrapper over `OnceLock` does not — that plant is checked in as the
// test's own control.
//
// [OB-15] IS CLEAN, checked before the row was written: `once_cell` is named in
// no manifest anywhere in this repository, so nothing it redirects was ever a
// differential oracle. `rustix 1.1.4` declares it as a WINDOWS DEV-dependency
// and never uses it, which is neither an oracle nor an edge in any cell.
// ---------------------------------------------------------------------------

// RE-MEASURED 2026-09-03 — two things that had moved without a re-measure,
// and one that moved in this change.
//
// (1) WORKSPACE 68 -> 69, resolved 115 -> 116, on mac-arm only: `aterm-objc`,
//     the first-party Objective-C runtime layer (2026-09-01, b90beb2d2), is a
//     path dependency and counts as a workspace member of the shipped graph.
//     No third-party package moved: third_party stays 47 on mac-arm and 188
//     on linux, and every other count is at ceiling.
// (2) THIRD-PARTY LOC, all three vendored-winit cells, from the W9-phase-2
//     port waves: tools/forge-budget.tsv was ratcheted to 563,759 (mac-arm)
//     with its long reason on 2026-09-02, but these constants were not
//     re-measured with it, and the tail of the wave (the cancelOperation: fix
//     and its siblings, +51 lines) landed after the ratchet. `winit`'s
//     dominator cost moved with it, 80,333 -> 82,708 (12 packages, unchanged).
//     Re-pinned once more in (4).
// (3) a966f37cc (the 2026-09-02 abort audit, one commit before this one): +42
//     lines in vendor/winit —
//     monitor.rs no longer `expect`s a display that vanished mid-enumeration,
//     window_delegate.rs no longer `unwrap`s a screen that is gone when
//     entering full screen, and `dragged_paths` checks the pasteboard's
//     classes before sending to them. Each carries a `// LOCAL PATCH (aterm):`
//     marker; see attest.rs for the marker count.
// (4) the 2026-09-04 self-audit of that commit: +5 lines in
//     vendor/winit/src/platform_impl/macos/window_delegate.rs — `dragged_paths`
//     now refuses the WHOLE list when any pasteboard element is not an
//     NSString (it had delivered the survivors, against its own comment), and
//     the set_fullscreen comment says the request is dropped. winit's
//     dominator cost 82,708 -> 82,713; every cell that resolves winit
//     (mac-arm, linux, win) moved by the same +5.
// (5) the 2026-09-05 ObjC exception containment: +308 lines in vendor/winit —
//     every send prototype re-spelled `extern "C-unwind"`, the observer and
//     queued-closure blocks moved onto aterm_objc::RcBlock, fullscreen rows
//     writing state before their sends, `@abort_on_exception` reasons on the
//     rows whose zero is not inert, and the `cached_modifiers` seam the app's
//     containment hook reads instead of a WindowServer-backed probe. winit's
//     dominator cost 82,713 -> 83,021, which now LEADS the mac-arm ranking
//     (objc2-app-kit stays at 82,976); each winit-resolving cell moved +308.
//     Fifteen new `// LOCAL PATCH (aterm):` markers (90 -> 105; attest.rs).
//
//   mac-arm  563,759 (ratchet) -> 563,852; linux 2,738,950 -> 2,739,043;
//            win 3,586,179 -> 3,586,272 — the same +93 on each cell that
//            vendors winit, which is the evidence it is one edit.
// (6) 2026-09-05, THE objc2 EXIT — the W12 + W13 merge, landed on top of the
//     containment in (5). The winit fork's last eight macOS files
//     (app.rs, app_state.rs, event_loop.rs, menu.rs, monitor.rs, window.rs,
//     aterm_objc_seam.rs, platform/macos.rs) and aterm-gui's last five
//     (alert_keys.rs, menu.rs, lib.rs's paste sheet, app_introspect.rs,
//     appkit.rs) are on aterm-objc — `SwizzleSite` swizzles `sendEvent:` with
//     the containment's stop_app_on_panic-outside/contain-inside order kept,
//     `MainThreadBound` replaces objc2-foundation's — and every objc2-family
//     row in both manifests is retired (`block2`'s had already left in (5)).
//     SEVEN PACKAGES LEAVE mac-arm: objc2-app-kit, objc2-foundation, objc2,
//     block2, objc2-encode, objc-sys and dispatch — 47 -> 40 third-party,
//     116 -> 109 resolved, 564,165 -> 391,435 LOC (-172,730), 10 -> 9 build
//     scripts. The fork itself is +288 lines over (5) — the swizzle and
//     MainThreadBound ports and their `// LOCAL PATCH (aterm):` markers
//     (105 -> 121; attest.rs) — so winit's dominator cost is 83,021 ->
//     83,309 and the two cells that vendor winit without the family move by
//     exactly that: linux 2,739,356 -> 2,739,644; win 3,586,585 ->
//     3,586,873. `objc2-app-kit` (1 / 82,976) and `objc2-foundation`
//     (2 / 60,733) leave the mac-arm ranking; `rustybuzz` (7 / 47,712) and
//     `serde` (3 / 38,412) enter at four and five. Measured on macOS with the
//     merged tree compiled, its test suites green and its eight drivers run.
// (7) the same day's objc-w7 residual: the `+arrayWithObjects:count:` SAFETY
//     comment in vendor/winit window_delegate.rs grew from one line to two
//     (`@@:r^@Q`, and why). +1 winit line; every winit-resolving cell +1.
// (8) 2026-09-07, the macOS 14 launch fix: +22 lines in vendor/winit, all of
//     them comment — app_state.rs (+19) says why the application delegate's
//     `NSApplicationDelegate` claim is true on a host whose AppKit does not
//     register the protocol (aterm-objc supplies a name-only one; v0.72.0
//     through v0.75.0 asserted the host had it and died at launch on 14.4.1)
//     and what AppKit actually asks of that delegate; window.rs (+3) names the
//     host its NSWindow layout numbers were measured on. No code line moved
//     and no `// LOCAL PATCH (aterm):` marker was added (attest.rs stays at
//     121). winit's dominator cost 83,310 -> 83,332; mac-arm 391,436 ->
//     391,458, linux 2,739,645 -> 2,739,667, win 3,586,874 -> 3,586,896 —
//     the same +22 on each cell that vendors winit.
// (9) 2026-09-09, the Rust-lane commit 907ef0bf6: `aterm help rust` MEASURES
//     which toolchain a directory actually gets — it calls
//     `aterm_verify::toolchain::Toolchain::discover`, reads the atpkg store's
//     winner and the refused candidates, and runs the resolved `rustc`/`targo`
//     for their versions — so `crates/aterm-cli` declares `aterm-verify`. That
//     crate has NO dependencies of its own (deliberately, and its manifest says
//     so), which is why this is the narrowest possible movement: +1 workspace
//     node and +1 resolved node on every cell that resolves aterm-cli, and
//     nothing else. third_party is untouched on all three (40 / 188 / 91), and
//     so is every third_party_loc, every build-script and proc-macro count, and
//     every duplicate-name count. mac-arm 109 -> 110, linux 259 -> 260, win
//     160 -> 161; workspace 69 -> 70, 71 -> 72, 69 -> 70. The wasm modules are
//     rooted at the engine, not at the `aterm` binary, so they never resolve
//     aterm-cli and do not move.
//
//     WHY THIS SAT UNMEASURED FOR TWO DAYS, which is the part worth recording:
//     three gates (0.79.0 and both 0.80.0 attempts) were read as "forge green"
//     when aterm-forge had never RUN. The workspace test stage aborted earlier
//     at `-p aterm-conformance --test paint`, whose rows were flaking under
//     gate load, and a stage that stops at the first failing binary never
//     reaches the later ones. The tell was in the logs all along: 3, then 4,
//     then 11 mentions of forge — the first run that reached it is the first
//     run that disagreed. An ABSENCE OF FAILURE IS NOT A PASS, and a ratchet
//     that is never executed guards nothing.
//
//     Console-life integration adds a direct aterm-effects -> aterm-types
//     dependency, but core/render already resolve it normally; this adds no node.
//
// (10) 2026-09-10, THE FABRIC IN THE WORKSPACE — v0.81.0's headline change,
//     and the first round in this file that GROWS the surface. `crates/
//     aterm-link` carried its own `[workspace]` table and path-depended on
//     `../../astream`, a sibling checkout most clones do not have, so
//     `cargo build` never built it and no release ever shipped the bridge. It
//     is an ordinary member now and the astream crates it needs are vendored
//     under `vendor/astream/crates/` (wire, cap, broker; aead is vendored too,
//     but the `sealed` feature that reaches it is off by default, so no cipher
//     tree is in any cell). See CHANGELOG.md `[0.81.0]`.
//
//     SEVEN THIRD-PARTY PACKAGES ENTER — NOT TEN, and the difference is a
//     CLASSIFIER FIX, not a re-measurement of the same question. The first
//     pass of this note read ten, because `loc::measure` decided
//     `is_third_party` from a DIRECTORY PREFIX: everything not under `crates/`
//     was somebody else's code. `vendor/astream` was then a copy of (and
//     since 2026-09-24 is a git submodule of)
//     `github.com/alabsystems/astream`, whose owner is this repository's
//     owner — the same account, not a similar name — vendored for a BUILD
//     reason and reached by `path = …`, so the prefix rule billed 3 packages
//     and 11,122 lines of ATERM'S OWN CODE to the surface this crate exists to
//     shrink, and `[OB-1]` demanded a `[patch.crates-io]` entry that cannot
//     exist (a patch entry replaces a REGISTRY package; these are not
//     published). [`crate::provenance`] is the third shape, and the question
//     is asked of it now: `crates/` OR a roster row is first-party, and the
//     five real forks under `vendor/` (winit, indexmap, libm, pkg-config,
//     smol_str) are third-party exactly as before.
//
//     What genuinely crossed the line is the `sha2` chain `astream-cap` mints
//     capabilities with: sha2, digest, block-buffer, crypto-common,
//     generic-array, typenum and cpufeatures, priced as ONE prize by
//     `blame sha2 --cell mac-arm` at dom 7 packages / 48,869 LOC because
//     `astream-cap` is sha2's only parent here. `sha2` was already a
//     Cargo.lock entry; this is the first time it has been in a shipped GRAPH,
//     and `crates/aterm-digest` exists precisely because sha2 + hmac cost
//     eight packages to expose four methods. The campaign has re-bought seven
//     of them, and one edge — routing the mint through aterm-digest — takes
//     all seven and all 48,869 lines back off every cell at once.
//
//       mac-arm  110 -> 121 resolved, 70 -> 74 workspace, 40 -> 47
//                third-party, 391,458 -> 440,327 LOC, 9 -> 10 build scripts
//       linux    260 -> 271, 72 -> 76, 188 -> 195,
//                2,739,667 -> 2,788,536, 31 -> 32
//       win      161 -> 172, 70 -> 74, 91 -> 98,
//                3,586,896 -> 3,635,765, 19 -> 20
//
//     +48,869 LOC to the line on all three, which is the evidence it is one
//     edit and not three: every one of the seven is target-independent source.
//     `workspace` gains FOUR on each cell, not one — `aterm-link` plus the
//     three astream crates, which are aterm's own and are counted as such.
//     `resolved` therefore moves by 11 while `third_party` moves by 7. The one
//     added build script is `generic-array`'s (checked against the registry
//     sources: none of the other six carries one, and no astream crate does).
//     Proc macros and duplicate names do not move on any cell. The two browser
//     modules are rooted at `aterm-wasm` and `aterm-gpu-web`, never reach
//     `aterm-link`, and are UNCHANGED to the line — which is why [`WASM_CPU`]
//     and [`WASM_GPU`] are not touched in this round.
//
//     ONE ROW ENTERS [`MAC_ARM_DOMINATORS`]: `sha2` at four, displacing
//     `serde` (3 / 38,412). `rustybuzz` did not move by a line and is still
//     there, one rank lower. `astream-cap` is NOT a row — `dominator::ranked`
//     ranks third-party packages, and the mint is aterm's own; what it drags
//     in is what gets billed. `winit`, `rustls` and `syn` are unchanged.
//
//     THE RATCHET REFUSED THIS, WHICH IS THE RATCHET WORKING, and the raise it
//     eventually recorded is the NARROW one. `budget` only ever lowers a
//     ceiling on its own, so the ten rows that grew were raised through
//     `budget --update --allow-regress`, each carrying the reason in the
//     file's fourth column where every run reprints it. Nine are the three
//     shipped cells x (third_party_packages, third_party_loc, build_scripts),
//     at +7 / +48,869 / +1 — not the +10 / +59,991 / +1 the prefix rule
//     produced, because a ceiling raised to cover first-party code stays loose
//     forever and this is the number this repository is least willing to let
//     drift. The tenth is `lock third_party_packages`, 499 -> 520, and none of
//     that 21 is first-party either: measured against
//     `git show 38f61d5be^:Cargo.lock`, the fabric added 26 lock entries, of
//     which 5 are source-less path packages that already counted as aterm's
//     own (the four astream crates and aterm-link) and 21 are the SEALED
//     TRANSPORT in full — chacha20poly1305 and the x25519/ed25519/curve25519
//     stack — which a lockfile records because it records every OPTIONAL
//     resolution and which no default build compiles. The `sha2` chain is
//     notably NOT among them: it was already in the lock, which is exactly how
//     it could enter a cell without moving that row.
//
//     THESE TWELVE WERE RED FOR TWO DAYS, for the reason note (9) records one
//     round earlier: the gate's test stage stopped at the first failing binary
//     and never reached aterm-forge. That hole is closed (`--no-fail-fast`,
//     merged 2026-09-10), which is the only reason this round happened before
//     a cut instead of after one.
//
// RE-MEASURED 2026-09-14 — THE FABRIC LEFT THE WINDOWS CELL, and only that
//     cell moves. `aterm-link` became a `[target.'cfg(unix)'.dependencies]`
//     entry of crates/aterm (the bridge is Unix-domain sockets and inherited
//     descriptors end to end, and the unconditional row had made the shipped
//     Windows binary unbuildable since v0.82.0 — `std::os::unix` at module
//     level in three of its files), so on x86_64-pc-windows-msvc the resolver
//     no longer reaches aterm-link, the three first-party astream crates, or
//     the sha2 chain astream-cap alone dragged in:
//
//       win      172 -> 161 resolved, 74 -> 70 workspace, 98 -> 91
//                third-party, 3,635,765 -> 3,586,896 LOC, 20 -> 19 build scripts
//
//     which is the v0.81.0 raise above undone to the line on this one cell —
//     -11 resolved, -4 workspace, -7 / -48,869 / -1 — and the 2026-09-10
//     numbers exactly. mac-arm and linux still carry the fabric and do not move
//     by a line; the two browser modules never reached it. The `link` verb on
//     Windows refuses by name (crates/aterm/src/main.rs `link_unavailable`)
//     instead of failing to exist.
//
//     MEASURED ON A WINDOWS HOST, the first time `cargo forge` ran on one, and
//     it could not until the same change: `resolve::abs_root` canonicalised the
//     workspace root, Windows answers the verbatim `\\?\C:\…` spelling, and
//     cargo then refused every cell under `--locked` with "cannot update the
//     lock file" against a current lock. The prefix is now stripped there.
//
//     THE SAME CHANGE LOWERED `lock third_party_packages` 520 -> 513, for an
//     unrelated reason recorded on that row: the `embed-resource`
//     build-dependency left with crates/aterm-winres (its msvc-only `vswhom`
//     chain needed `libc` items the first-party libc withholds on Windows),
//     taking seven registry packages out of the lock. No cell's `-e normal`
//     graph moves for that: build-dependencies were never in these numbers.
//
// RE-MEASURED 2026-09-14 — `aterm-phase` ENTERED EVERY SHIPPED CELL, one
//     workspace crate and nothing else. c1fc82257 (round 13) moved the
//     worker-phase reader out of `aterm-agent/src/supervise` into
//     `crates/aterm-phase` so the fabric bridge's presence rows could carry
//     `phase=` without linking the supervisor; `aterm-agent` and `aterm-link`
//     both depend on it, and the shipped root reaches `aterm-agent` on every
//     cell (`aterm fleet` / `aterm drive`), so:
//
//       mac-arm  121 -> 122 resolved, 74 -> 75 workspace
//       linux    271 -> 272, 76 -> 77
//       win      161 -> 162, 70 -> 71
//
//     Nothing else moves by a line on any cell: the crate declares NO
//     dependencies (its manifest says that is the point of it), so
//     third-party packages, LOC, build scripts, proc macros and duplicate
//     names all stand, and the two browser modules never reach `aterm-agent`,
//     so [`WASM_CPU`] and [`WASM_GPU`] are untouched. The commit that added
//     the crate did not touch this file, and the six baseline tests were red
//     at origin/main from its merge until this note.
//
// RE-MEASURED 2026-09-23 — `aterm-messages` ENTERED EVERY NATIVE CELL, one
//     workspace crate and nothing else. The unified message system
//     (docs/DESIGN-unified-messages-2026-09-21.md) put its platform-neutral
//     model — the message center, the band's width law, the log codec — in
//     `crates/aterm-messages`, and `aterm-gui` depends on it, so every cell
//     that ships the window moves by one. Measured with `cargo forge survey`
//     over all six native cells on this tree, not inferred from the edge:
//
//       mac-arm   122 -> 123 resolved, 75 -> 76 workspace
//       linux     272 -> 273, 77 -> 78
//       win       162 -> 163, 71 -> 72
//       mac-x64, linux-arm, win-arm: identical to their siblings, as always
//
//     Nothing else moves by a line on any cell: the crate's only dependency is
//     `aterm-time` (already in every graph), so third-party packages, LOC,
//     build scripts, proc macros and duplicate names all stand, and
//     `tools/forge-budget.tsv`, which ratchets only those, does not move. The
//     browser modules never reach `aterm-gui`, so [`WASM_CPU`] and
//     [`WASM_GPU`] are untouched.
//
// RE-MEASURED 2026-09-24 — `aterm-sysprobe` ENTERED EVERY NATIVE CELL, one
//     workspace crate and nothing else. The strain row (design §10.14,
//     ruling 210) reads the machine — Mach host statistics, `sysctlbyname`,
//     libproc and `NSProcessInfo` on macOS, pure `/proc` parsers for Linux —
//     through `crates/aterm-sysprobe`, and `aterm-gui` depends on it, so every
//     cell that ships the window moves by one. Measured with `cargo forge
//     survey` over all six native cells on this tree, not inferred from the
//     edge:
//
//       mac-arm   123 -> 124 resolved, 76 -> 77 workspace
//       linux     273 -> 274, 78 -> 79
//       win       163 -> 164, 72 -> 73
//       mac-x64, linux-arm, win-arm: identical to their siblings, as always
//
//     Nothing else moves by a line on any cell: its dependencies are
//     `aterm-messages` everywhere and, on macOS only, `libc` (the first-party
//     one) and `aterm-objc` — all three already in every graph that reaches
//     them — so third-party packages, LOC, build scripts, proc macros and
//     duplicate names all stand, and `tools/forge-budget.tsv` does not move.
//     The browser modules never reach `aterm-gui`, so [`WASM_CPU`] and
//     [`WASM_GPU`] are untouched.
//
// RE-MEASURED 2026-09-15 — THE winit FORK GREW BY 610 LINES, and nothing else
//     moved anywhere. Two commits landed in `vendor/winit` after the round-13
//     re-pin (dd444ac8b, the commit these constants were last measured at), and
//     both are aterm's own edits inside the fork — the shape notes (5), (7) and
//     (8) above keep recording, one more time:
//
//       c5326f2d8  feat(gui,winit): a native Wayland clipboard — copy, paste,
//                  copy-on-select and OSC 52 with no XWayland. +571 / -3 over
//                  nine files, the bulk of it the new 452-line
//                  platform_impl/linux/wayland/clipboard.rs.          net +568
//       b41e769b0  fix(gui,types,winit-keymap): the physical keypad reaches the
//                  keypad encoder — event.rs grows the keypad predicate.
//                  +45 / -3 in one file.                              net  +42
//
//     Measured directly, not inferred: physical `*.rs` lines under
//     `vendor/winit` go 63,600 (dd444ac8b) -> 64,168 (c5326f2d8) -> 64,210
//     (b41e769b0 = HEAD), and `git diff --numstat dd444ac8b..HEAD -- vendor/`
//     is +616 / -6 with EVERY path under `vendor/winit/`. No other fork moved.
//
//       mac-arm  440,327 -> 440,937
//       linux    2,788,536 -> 2,789,146
//       win      3,586,896 -> 3,587,506
//
//     THE SAME +610 ON ALL THREE, which is the evidence it is one fork's edits
//     and not three unrelated drifts — the identical fingerprint notes (4),
//     (5), (7) and (8) carry, and the wasm modules never resolve winit so
//     [`WASM_CPU`] and [`WASM_GPU`] do not move by a line.
//
//     NO DEPENDENCY CHANGED VERSION, and that was checked rather than assumed:
//     `Cargo.lock` against `git show dd444ac8b:Cargo.lock` moves exactly two
//     things — the workspace version 0.85.0 -> 0.86.0 on all 84 first-party
//     entries, and ONE added name, `aterm-winsign`, a path package no shipped
//     cell resolves. Not one third-party package's version or `source` line
//     differs. `vendor/winit/Cargo.toml` is byte-identical, so the clipboard
//     took no new dependency: `resolved`, `workspace`, `third_party`,
//     `build_scripts`, `proc_macros` and `duplicate_names` are UNCHANGED in
//     all five cells (122/75/47, 272/77/195, 162/71/91), and only the LOC rows
//     and `winit`'s dominator move. That is the 2026-08-30 shape: a
//     measurement of the same graph, with the fork's own source larger.
//
//     ONE DOMINATOR MOVED, by exactly the same 610: `winit` 12 / 83,332 ->
//     12 / 83,942, package count unchanged because the fork gained no edge.
//     It still LEADS the mac-arm ranking (`rustls` is second at 5 / 65,413),
//     so the order is untouched. No other anchor reaches winit and none moved.
//     `tools/forge-budget.tsv` is raised on the same three rows through
//     `--allow-regress`, with this cause in its fourth column.
// RE-MEASURED 2026-09-16 — THE FORK'S APACHE NOTICES, eight comment lines and
//     nothing else. `cargo forge attest` was red with four [OB-7] violations:
//     c5326f2d8 and b41e769b0 edited three files of `vendor/winit` and added a
//     fourth without the modification notice Apache-2.0 §4(b) requires. The
//     notices are two comment lines per file over four files — `src/platform/
//     wayland.rs`, `src/platform_impl/linux/wayland/mod.rs`, `.../seat/keyboard/
//     mod.rs` and the added `.../wayland/clipboard.rs` — so every cell that
//     resolves winit gains exactly 8 physical lines and nothing else moves:
//
//       mac-arm  440,937 -> 440,945
//       linux    2,789,146 -> 2,789,154
//       win      3,587,506 -> 3,587,514
//
//     THE SAME +8 ON ALL THREE, the same fingerprint the 2026-09-15 note
//     describes: one fork's own source, no package, version or edge. The wasm
//     modules never resolve winit, so [`WASM_CPU`] and [`WASM_GPU`] do not move.
//     `winit`'s dominator takes the same +8 (12 / 83,942 -> 12 / 83,950) and
//     still leads the mac-arm ranking. `tools/forge-budget.tsv` is raised on the
//     same three rows through `--allow-regress`, with this cause in its fourth
//     column.
// RE-MEASURED 2026-09-16 — THE WAYLAND CLIPBOARD'S CLAIM ORDER, one defect fix
//     in the fork and nothing else. A copy claimed the selection before it
//     released the one it already held, so a compositor dropped every second
//     copy in silence and the fork's own release then walked the clipboard back
//     to the PREVIOUS text; the fix releases first, refuses to release in front
//     of a claim the compositor will not take (no keyboard focus on the seat),
//     and answers the caller with the outcome instead of `true`. THREE files of
//     `vendor/winit` — `.../wayland/clipboard.rs` (+564 / -47, the bulk of it a
//     compositor model and the eight tests added around it, five of which drive
//     the real copy through it), `.../wayland/seat/mod.rs` (+25) and
//     `.../wayland/seat/keyboard/mod.rs` (+11) — so every cell that resolves
//     winit gains exactly 553 physical lines:
//
//       mac-arm  440,945 -> 441,498
//       linux    2,789,154 -> 2,789,707
//       win      3,587,514 -> 3,588,067
//
//     Measured directly, not inferred: physical `*.rs` lines under
//     `vendor/winit` go 64,218 -> 64,771 over the same 179 files, and
//     `git diff --numstat ce5c66bf6..HEAD -- vendor/` is +600 / -47 with every
//     path under `vendor/winit/`. THE SAME +553 ON ALL THREE, the fingerprint
//     notes (4), (5), (7), (8) and the two 2026-09-15/16 notes above describe:
//     one fork's own source, no package, version or edge. `vendor/winit/
//     Cargo.toml` is byte-identical and the fix took no new dependency, so
//     `resolved`, `workspace`, `third_party`, `build_scripts`, `proc_macros`
//     and `duplicate_names` are unchanged in all five cells. The wasm modules
//     never resolve winit, so [`WASM_CPU`] and [`WASM_GPU`] do not move.
//     `winit`'s dominator takes the same +553 (12 / 83,950 -> 12 / 84,503),
//     package count unchanged, and still leads the mac-arm ranking (`rustls` is
//     second at 5 / 65,413). `tools/forge-budget.tsv` is raised on the same
//     three rows through `--allow-regress`, with this cause in its fourth
//     column.
// RE-MEASURED 2026-09-24 — THE X11 KEY TIMESTAMP, one instrument seam in the
//     fork and nothing else. The X11 event processor publishes the server
//     `time` of the key event it is dispatching, for exactly the length of the
//     `KeyboardInput` callback, and `platform::x11::key_event_server_time`
//     reads it — so aterm can backdate a Linux key to the X server's stamp the
//     way macOS backdates by the NSEvent queue age (aterm-gui
//     `platform::current_event_queue_age_ns`). TWO files of `vendor/winit` —
//     `platform/x11.rs` (+22) and `platform_impl/linux/x11/event_processor.rs`
//     (+7), each with its Apache §4(b) notice — so every cell that resolves
//     winit gains exactly 29 physical lines:
//
//       mac-arm  441,498 -> 441,527
//       linux    2,789,707 -> 2,789,736
//       win      3,588,067 -> 3,588,096
//
//     Measured, not inferred: `targo --unverified forge budget` read exactly
//     +29 on the six native rows and nothing else, and the numstat of
//     `vendor/` is +29 / -0 with both paths under `vendor/winit/`.
//     `vendor/winit/Cargo.toml` is byte-identical, so every other field is
//     unchanged in all five cells, and the wasm modules, which never resolve
//     winit, do not move. `tools/forge-budget.tsv` is raised on the same six
//     rows through `--allow-regress`, with this cause in its fourth column.
// RE-MEASURED 2026-09-24 (later the same day) — THE SAME SEAM, MADE TESTABLE,
//     +15. A review found the stamp's lifecycle (set for exactly one dispatch,
//     cleared after, absent elsewhere) untested: winit's own tests cannot run
//     inside this workspace, and the store/clear pair sat inline in the event
//     processor. It is now one `#[doc(hidden)] pub` helper in
//     `platform/x11.rs` (+21 / -5) that also clears on unwind, called once from
//     `event_processor.rs` (+4 / -5), so aterm-gui drives the real seam in a
//     test. Net +15 on every cell that resolves winit:
//
//       mac-arm  441,527 -> 441,542
//       linux    2,789,736 -> 2,789,751
//       win      3,588,096 -> 3,588,111
//
//     Measured by `targo --unverified forge budget` (+15 on the same six
//     native rows, nothing else) and the `vendor/` numstat (+25 / -10).
// RE-MEASURED 2026-09-26 — THE WAKER IN A NESTED RUN LOOP, one defect fix in
//     the fork and nothing else (design ruling 267 of the unified-messages
//     design). While the ⌘Q confirmation (`NSAlert runModal`) stood inside the
//     event handler, a past-due `WaitUntil` left winit's `EventLoopWaker` timer
//     firing at its 0.1 µs interval in every common mode — 88 % CPU for as long
//     as the dialog was up, measured live on day two of round 16. Both run-loop
//     observers now stop the waker when they find the handler borrowed (only a
//     nested run loop can), and the outer turn's own `cleared` re-arms it.
//     ONE file of `vendor/winit` — `platform_impl/macos/app_state.rs` (+16 /
//     -0, one new helper and its `// LOCAL PATCH (aterm):` marker, so the
//     marker census moves 121 -> 122) — so every cell that resolves winit gains
//     exactly 16 physical lines:
//
//       mac-arm  441,542 -> 441,558
//       linux    2,789,751 -> 2,789,767
//       win      3,588,111 -> 3,588,127
//
//     `vendor/winit/Cargo.toml` is byte-identical, so every other field is
//     unchanged in all five cells, and the wasm modules, which never resolve
//     winit, do not move. `tools/forge-budget.tsv` is raised on the same six
//     rows through `--allow-regress`, with this cause in its fourth column.
// RE-MEASURED 2026-09-27 — THE WAKER IS aterm_objc::WakeTimer (f715ad5aa,
//     the 2026-09-26 hang: a late 0.1 µs repeating CFRunLoopTimer made
//     CoreFoundation walk ~10⁷ intervals with the run-loop lock held). TWO
//     files of `vendor/winit` — `platform_impl/macos/observer.rs` (+25 / -74,
//     the CFRunLoopTimer waker replaced by the first-party timer, and one more
//     `// LOCAL PATCH (aterm):` marker, 5 -> 6 in that file, so the marker
//     census moves 122 -> 123) and `platform_impl/macos/app_state.rs` (+5 /
//     -2, its 13 markers unchanged) — so every cell
//     that resolves winit loses exactly 46 physical lines, measured by `cargo
//     forge survey` on all six:
//
//       mac-arm / mac-x64      441,558 -> 441,512
//       linux / linux-arm      2,789,767 -> 2,789,721
//       win / win-arm          3,588,127 -> 3,588,081
//
//     `vendor/winit/Cargo.toml` is byte-identical. The six ceilings in
//     `tools/forge-budget.tsv` are lowered to match by `cargo forge budget
//     --update` (a ceiling above its measured row fails the ratchet-agreement
//     test; each row keeps the reason of its last raise). The commit
//     that made the edit did not re-pin these, and `aterm-forge`'s
//     `the_real_tree_reproduces_the_measured_marker_floor` and the two survey
//     totals tests caught it on the next run, as they are meant to.
// RE-MEASURED 2026-09-27 — THE FORK'S windows-sys 0.52 -> 0.61 (aaf606af7,
//     docs/THIRD_PARTY_ROAD_TO_ZERO.md). The Windows backend's handles became
//     opaque pointers, so `vendor/winit` gained typed handle conversions
//     (`platform_impl/windows/handle.rs`) and the Send/Sync impls the pointer
//     types no longer derive: +78 physical lines in every cell that resolves
//     winit. On x86_64 Windows the move also took windows-sys 0.52 out of the
//     graph (-3 packages / -384,578 lines / -1 build script / its duplicate
//     name); ARM Windows keeps 0.52 through ring 0.17's aarch64 CPU-feature
//     probe, so there only the +78 moves. Measured by `cargo forge survey` on
//     all six (`resolved` moves with `third_party`):
//
//       mac-arm / mac-x64      441,512 -> 441,590
//       linux / linux-arm      2,789,721 -> 2,789,799
//       win                    3,588,081 -> 3,203,581   (91 -> 88 packages)
//       win-arm                3,588,081 -> 3,588,159
//
//     The commit that moved the fork re-pinned the budget but not these, and
//     `aterm-forge`'s ratchet-agreement, `*_stays_within_the_measured_baseline`
//     and survey-totals tests caught it once the gate ran them.
pub const MAC_ARM: Baseline = Baseline {
    cell: "mac-arm",
    resolved: 124,
    workspace: 77,
    third_party: 47,
    third_party_loc: 441_590,
    build_scripts: 10,
    proc_macros: 2,
    duplicate_names: 1,
};

pub const LINUX: Baseline = Baseline {
    cell: "linux",
    resolved: 274,
    workspace: 79,
    third_party: 195,
    third_party_loc: 2_789_799,
    build_scripts: 32,
    proc_macros: 16,
    duplicate_names: 6,
};

pub const WIN: Baseline = Baseline {
    cell: "win",
    resolved: 161,
    workspace: 73,
    third_party: 88,
    third_party_loc: 3_203_581,
    build_scripts: 18,
    proc_macros: 7,
    duplicate_names: 0,
};

/// The CPU browser module, `crates/aterm-wasm` — the engine plus the
/// `aterm-render` rasterizer, blitted with `putImageData`.
///
/// SCOPE CORRECTED 2026-08-30, and this row is NOT comparable to the `wasm`
/// row it replaces. The old row was rooted at the `aterm` BINARY, which is a
/// `[[bin]]` nothing compiles for wasm32; it read 81 packages / 1,172,582 lines
/// of a configuration that is never built. See
/// [`crate::resolve::default_cells`] for what that counted and what it missed.
///
/// The first honest fall of this row was `getrandom` on 2026-08-30: 27 -> 25
/// packages, 255,841 -> 246,067 lines. `getrandom 0.2` with `features = ["js"]`
/// was declared by FOUR aterm manifests — aterm-shell-integration (a wasm32 arm
/// of the capability-nonce mint that no browser build can call: `generate_nonce`
/// has one caller, and it spawns a PTY) plus aterm-wasm, aterm-gpu-web and
/// aterm-effects-web, each justified in a comment as "harmless if unused". They
/// were the ONLY parents `cargo tree -i getrandom` found on either browser cell,
/// so the defensive rows were the whole dependency; `js-sys` came out with it
/// here, and stayed on wasm-gpu where web-sys and wgpu hold it.
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

/// The GPU browser module, `crates/aterm-gpu-web` — the same engine plus
/// `aterm-gpu` over `wgpu`'s WebGL2 backend.
///
/// The gap to [`WASM_CPU`] is 37 third-party packages / 729,087 lines, all of
/// it inside `wgpu`'s SUBTREE — and a subtree is not a removal price. The
/// dominator (`blame wgpu --cell wasm-gpu`) is **33 packages / 523,700 lines**:
/// `web-sys` (199,507 lines, the largest single package in the gap),
/// `raw-window-handle` and `wasm-bindgen-futures` each have a direct edge from
/// `aterm-gpu-web` and survive `wgpu`'s retirement. Outside the gap the two
/// graphs differ by exactly one node, each module's own root.
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
// THE SECOND ARCHITECTURE OF EACH SHIPPED OS — measured 2026-09-18 on
// m17-tower, the day these three triples became cells.
//
// EACH ONE READ EXACTLY LIKE ITS SIBLING, every field, and that is the fact
// worth recording rather than a coincidence worth hiding (until 2026-09-25,
// when ARM Windows became the first to differ: see [`WIN_ARM`]): `cargo tree`'s
// per-target resolve keys on `target_os` and `target_family` almost everywhere
// in this graph, so the ARM Linux surface is the x86_64 Linux surface and the
// Intel-Mac surface is the Apple-Silicon one. What the rows buy is the day that
// STOPS being true — an `#[cfg(target_arch)]`-gated dependency, a vendored fork
// with an arch-specific edge, an assembly crate pulled in on one arch only —
// which under one ceiling per OS would have been invisible. `cargo forge survey
// --cell mac-x64 --cell linux-arm --cell win-arm` prints all three.
// ---------------------------------------------------------------------------

/// The Intel-Mac slice of the universal release binary (`aterm-release`'s
/// buildplan.rs builds it under upstream stable). Identical to [`MAC_ARM`] in
/// every field.
pub const MAC_X64: Baseline = Baseline {
    cell: "mac-x64",
    resolved: 124,
    workspace: 77,
    third_party: 47,
    third_party_loc: 441_590,
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
    third_party_loc: 2_789_799,
    build_scripts: 32,
    proc_macros: 16,
    duplicate_names: 6,
};

/// ARM Windows — the triple apps/aterm-win/build.ps1 selects for itself on an
/// ARM64 host. Identical to [`WIN`] but for windows-sys 0.52, which ring 0.17's
/// aarch64 CPU-feature probe still resolves here and nothing does on x86_64
/// since the fork's 0.61 move (2026-09-25): +3 packages, +384,578 lines, +1
/// build script and the duplicate name.
pub const WIN_ARM: Baseline = Baseline {
    cell: "win-arm",
    resolved: 164,
    workspace: 73,
    third_party: 91,
    third_party_loc: 3_588_159,
    build_scripts: 19,
    proc_macros: 7,
    duplicate_names: 1,
};

/// The eight cells, in [`crate::resolve::default_cells`] order, so a test that
/// already holds a cell index can index this too. APPEND-ONLY for that reason:
/// the three 2026-09-18 rows sit at the end rather than beside their siblings.
pub const CELLS: [Baseline; 8] = [
    MAC_ARM, LINUX, WIN, WASM_CPU, WASM_GPU, MAC_X64, LINUX_ARM, WIN_ARM,
];

/// The names duplicated in the mac-arm cell. Pinned as NAMES rather than a
/// count because which crate is doubled is the actionable half of the fact.
/// THE FLIP shrank this to ONE: `block2`, `objc2` and `objc2-foundation`
/// each resolved twice only because wgpu-hal held the 0.3/0.6 generation
/// beside winit's 0.2/0.5 one — the whole doubled half was wgpu's, and it
/// left with the graph. `bitflags` remains (1.3.2 under core-graphics beside
/// 2.x everywhere else), exactly the survey's one dedup row.
pub const MAC_ARM_DUPLICATE_NAMES: [&str; MAC_ARM.duplicate_names] = ["bitflags"];

/// `hashbrown`'s version count, pinned separately because it was for a long
/// time the worst duplicate in the cell — THREE live versions in one binary,
/// then two.
///
/// **It is ONE now, and this constant is redundant with the NAMES assert, which is the actual tooth: a second hashbrown version is by definition a duplicate NAME, so `MAC_ARM_DUPLICATE_NAMES` fires first and this constant can never be the failing assertion. Kept as documentation of the count, credited honestly.** The
/// second copy was `hashbrown 0.17.1`, held by exactly one requirement in the
/// whole graph: upstream `indexmap 2.14`'s `hashbrown = "0.17"`. Nothing else
/// on any cell asked for 0.17, and every other `hashbrown` parent — `naga`,
/// `wgpu`, `wgpu-core`, `wgpu-hal` — was already on 0.16.1, so aterm shipped
/// the whole of hashbrown twice to satisfy one vendored manifest line.
/// `vendor/indexmap/Cargo.toml` now asks for `"0.16"` and the duplicate is
/// gone: −1 package / −25,236 LOC / −496 unsafe tokens on mac-arm, linux, win
/// and wasm-gpu alike (wasm-cpu never had indexmap).
///
/// Pinned as a COUNT OF RESOLVED VERSIONS rather than as a lookup into the
/// duplicate map, because the duplicate map no longer has a `hashbrown` key at
/// all — a lookup would panic on the fix rather than assert it. The version
/// that survives is the shared one, which is why this dedup cost a version
/// string and not a port.
///
/// hashbrown is also no longer the biggest dedup prize on mac-arm; that is
/// `objc2-foundation` at 59,492 LOC (0.2.2 beside 0.3.2). That ordering flips
/// again the day `wgpu` leaves, because the 0.3.2 copy is wgpu's alone — which
/// is the point of pinning the NAMES and not just the count.
/// ZERO since THE FLIP: every `hashbrown` parent on this cell — naga, wgpu,
/// wgpu-core, wgpu-hal, indexmap under them — left with the wgpu graph, so
/// the package is not resolved here at all. (The ordering note above about
/// the biggest dedup flipping "the day wgpu leaves" resolved itself: the
/// whole `objc2-foundation` duplicate left too.)
pub const MAC_ARM_HASHBROWN_VERSIONS: usize = 0;

// --------------------------------------------------------- dominator anchors
//
// RETIRED 2026-09-25: `Dom` and its five records (`MAC_ARM_DOMINATORS`,
// `LINUX_DOMINATORS`, `MAC_ARM_UREQ`, `UREQ_RE_PARENTED`, `UREQ_DESIGN_NOTE`)
// had no reader — no test had re-asserted them since 2026-09-24, and the
// three ureq rows described a graph no checkout has had since ureq left on
// 2026-09-10. The round notes above that name them are history; the live
// measure is `aterm-forge blame <pkg> --cell <cell>`, and the cell ceilings in
// `loc` do the bounding.

/// THE TIE BETWEEN THIS FILE AND `tools/forge-budget.tsv`, read without a
/// `cargo tree` (the TSV alone, in milliseconds).
///
/// The repository keeps the same measurement in two places, and only this one
/// is under an automatic gate. On 2026-08-30 they had drifted 12 to 14
/// packages apart on every cell with nobody told, and two judged escapes
/// followed: deleting a scope's rows from the TSV outright (the whole
/// `wasm-gpu` scope) left the gate GREEN — the figures fall to an advisory
/// "UNRATCHETED" list — and the metrics nothing compared could be hand-raised
/// in the TSV alone, bypassing `--allow-regress`'s reason rule.
///
/// The relation held is `TSV ceiling <= measured row`, not equality: a
/// retirement lowers the TSV (`--update`) and leaves this file alone. The
/// other direction — a const here raised with the TSV left where it was — is
/// caught by `loc`'s cell tests, which hold the live graph under both.
#[cfg(test)]
pub(crate) mod ratchet_agreement {
    use super::{Baseline, CELLS};
    use crate::budget::{self, Row};

    /// The ratchet's scope string for each [`CELLS`] row, in order. Not derived
    /// from the triple: `wasm32-unknown-unknown` carries two cells, so the
    /// browser modules append their handle (`budget::scope_of`). Spelled out
    /// rather than recomputed so a change to either side of the pairing is a
    /// diff.
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
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(std::path::Path::parent)
            .expect("crates/aterm-forge sits two levels under the workspace root");
        budget::load(root).unwrap_or_else(|e| panic!("{}: {e}", budget::BUDGET_PATH))
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
