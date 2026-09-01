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

pub const MAC_ARM: Baseline = Baseline {
    cell: "mac-arm",
    resolved: 115,
    workspace: 68,
    third_party: 47,
    third_party_loc: 561_477,
    build_scripts: 10,
    proc_macros: 2,
    duplicate_names: 1,
};

pub const LINUX: Baseline = Baseline {
    cell: "linux",
    resolved: 259,
    workspace: 71,
    third_party: 188,
    third_party_loc: 2_736_668,
    build_scripts: 31,
    proc_macros: 16,
    duplicate_names: 6,
};

pub const WIN: Baseline = Baseline {
    cell: "win",
    resolved: 160,
    workspace: 69,
    third_party: 91,
    third_party_loc: 3_583_897,
    build_scripts: 19,
    proc_macros: 7,
    duplicate_names: 1,
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

/// The five cells, in [`crate::resolve::default_cells`] order, so a test that
/// already holds a cell index can index this too.
pub const CELLS: [Baseline; 5] = [MAC_ARM, LINUX, WIN, WASM_CPU, WASM_GPU];

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
    /// change worth seeing. `Some` is required for a duplicated name: five of
    /// them exist on mac-arm ([`MAC_ARM_DUPLICATE_NAMES`]). None of the five
    /// anchors is currently a duplicated name, so every one carries `None` —
    /// which means the anchors also assert, as a side effect, that no anchor has
    /// silently started resolving twice.
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
/// `softbuffer` is the FOURTH instance of the growth shape above, the largest
/// so far, and the one that finally cost this list its `libc` row. Retiring
/// softbuffer on the macOS cell (2026-08-30) removed 2 packages / 27,628 LOC
/// from the total — and moved `wgpu` from 32 / 460,964 to **38 / 660,197**,
/// because the `objc2` 0.3.x/0.6.x stack was a JOINT hostage reachable through
/// both and therefore billed to NEITHER. With one parent gone it transferred
/// wholesale: 199,233 lines and 6 packages, to the line. `wgpu` now holds 43% of
/// this cell's packages and 53% of its lines by itself.
///
/// The same retirement reordered the head twice over. `wgpu-hal` (8 / 251,320)
/// rose to rank two for exactly the same reason — it is where the objc2 stack
/// actually attaches — and `winit` (12 / 78,956) took the fifth slot.
///
/// `libc` is GONE from this list, and it left the SURFACE, not just the head:
/// `blame libc --cell mac-arm` now reports `dom 0 package(s) / 0 LOC` and names
/// its source as `crates/aterm-libc — PATCHED path package. aterm OWNS and
/// maintains this copy.` A first-party patch target is a workspace member, so it
/// is not third-party and cannot have a third-party dominator. Its old pin of
/// 1 / 127,772 was the largest single stale row in this file.
///
/// ONE MEASUREMENT TRAP is worth recording, because it silently under-reported
/// this by a factor of seven: `loc::package_dir` used to resolve
/// `<name>-<version>` against the registry checkout BEFORE the workspace, which
/// is right for a vendored fork (the pristine copy is the surface) and wrong
/// for a first-party replacement — it measured our 1,541-line crate as
/// upstream's 72,271-line one, and the win read as 12,212 lines. Workspace
/// members now win, by manifest name.
/// `wgpu` SHRANK here for the first time, and by a row that is not its own:
/// 38 packages / 660,280 LOC → **37 / 635,044** when `vendor/indexmap` moved
/// its `hashbrown` requirement from `"0.17"` to `"0.16"`. `hashbrown 0.17.1`
/// was a leaf whose only parent was `indexmap`, and `indexmap`'s only parents
/// on this cell are `naga` and `wgpu-core` — both inside wgpu's cost — so the
/// dedup came off wgpu's dominator to the line, 25,236 for 25,236, and off no
/// other anchor. `wgpu-hal` and `naga` do not move: neither dominated
/// `indexmap` alone, which is the same two-parents-bill-neither shape this
/// list keeps recording, running for once in the direction of a shrink.
///
/// RE-PINNED AT THE FLIP (2026-08-31): `wgpu` (37 / 635,044), `wgpu-hal`
/// and `naga` are GONE from the graph — the campaign's prize collected in
/// full, asserted as ABSENCE in `dominator::tests` (the libc/AccessKit
/// shape: a zero cost would also be reported for a package forge failed to
/// see). What leads now is the AppKit/window stack and the updater's TLS:
/// `objc2-app-kit` unchanged at the top, `winit` up by exactly the fork's
/// own +664 lines of edits (the headless arm + the §4(b) notices),
/// `rustls`, `syn`, and the 0.2-generation `objc2-foundation` (whose 0.3
/// twin left with wgpu-hal).
pub const MAC_ARM_DOMINATORS: [Dom; 5] = [
    Dom {
        name: "objc2-app-kit",
        version: None,
        pkgs: 1,
        loc: 82_976,
    },
    Dom {
        name: "winit",
        version: None,
        pkgs: 12,
        loc: 80_333,
    },
    // RE-PINNED 2026-09-01 by the `once_cell` row, and it is the first time a
    // first-party patch target has moved an anchor in this file. `rustls` is
    // `once_cell`'s ONLY parent on mac-arm, so the leaf sat inside rustls's
    // dominator; retiring it takes exactly 1 package / 3,950 lines off this
    // number and nothing else. The ORDER is unchanged — rustls stays third.
    Dom {
        name: "rustls",
        version: None,
        pkgs: 5,
        loc: 65_413,
    },
    Dom {
        name: "syn",
        version: None,
        pkgs: 1,
        loc: 64_931,
    },
    Dom {
        name: "objc2-foundation",
        version: None,
        pkgs: 2,
        loc: 60_733,
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

#[cfg(test)]
mod ratchet_agreement {
    use super::*;

    /// THE TRIPWIRE THAT WAS MISSING. This repository keeps the same
    /// measurement in TWO places — the ceilings in `tools/forge-budget.tsv`
    /// (enforced by `cargo run -p xtask -- gate forge`) and the baselines above
    /// (enforced only by `cargo test -p aterm-forge`) — and only one of them is
    /// in the required gate.
    ///
    /// So they drifted, and nobody was told. Every retirement wave ran
    /// `cargo forge budget --update` and left this file alone, until on
    /// 2026-08-30 the baselines were **12 to 14 packages stale on every cell**:
    /// mac-arm claimed 101 packages / 1,487,430 LOC against a live 89 /
    /// 1,248,254, and `cargo test -p aterm-forge` had been RED with 13 failures
    /// for several waves. The largest single stale row asserted a dominator for
    /// `libc`, which by then was not third-party at all — it had been retired to
    /// the workspace member `crates/aterm-libc`.
    ///
    /// The two files are re-derived from the same live graph by the same code,
    /// so EQUALITY is the honest relation, not "ceiling >= baseline". Slack
    /// between them is exactly the state that hid the drift. A wave that
    /// ratchets one and forgets the other now fails here immediately, naming
    /// both numbers.
    #[test]
    fn the_ratchet_and_these_baselines_are_the_same_measurement() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(std::path::Path::parent)
            .expect("crates/aterm-forge sits two levels under the workspace root")
            .to_path_buf();
        let rows = match crate::budget::load(&root) {
            Ok(r) => r,
            // A checkout with no ratchet file yet is not a drift.
            Err(_) => return,
        };
        if rows.is_empty() {
            return;
        }

        // The ratchet's scope strings, in CELLS order. Not derived from the
        // triple: wasm32-unknown-unknown carries two cells, so the triple stopped
        // being a unique key for a cell on 2026-08-30 and the two browser modules
        // append their handle (`budget::scope_of`). Spelled out here rather than
        // recomputed so that a change to either side of the pairing is a diff.
        let scopes = [
            ("mac-arm", "shipped.aarch64-apple-darwin"),
            ("linux", "shipped.x86_64-unknown-linux-gnu"),
            ("win", "shipped.x86_64-pc-windows-msvc"),
            ("wasm-cpu", "shipped.wasm32-unknown-unknown.wasm-cpu"),
            ("wasm-gpu", "shipped.wasm32-unknown-unknown.wasm-gpu"),
        ];
        for (base, (cell, scope)) in CELLS.iter().zip(scopes) {
            assert_eq!(base.cell, cell, "CELLS order changed");
            let scope = scope.to_string();
            let ceiling = |metric: &str| -> Option<u64> {
                rows.iter()
                    .find(|r| r.scope == scope && r.metric == metric)
                    .map(|r| r.ceiling)
            };
            // EVERY metric, and the row must EXIST. Two judged escapes forced
            // both halves: with `if let Some` arms, deleting a cell's rows from
            // the TSV outright left the gate GREEN (the figures fell to an
            // advisory "UNRATCHETED" list) and this suite at 149/0 — the whole
            // `wasm-gpu` scope could vanish unbounded; and the three metrics the
            // arms did not cover (`build_scripts`, `proc_macros`,
            // `duplicate_names`) could be hand-raised in the TSV alone,
            // bypassing the >=80-char-reason rule, with nothing going red.
            let require = |metric: &str, want: u64| {
                let got = ceiling(metric).unwrap_or_else(|| {
                    panic!(
                        "{cell}: tools/forge-budget.tsv has NO `{metric}` row for scope \
                         `{scope}`. A measured cell with no ratchet row is a SILENT GREEN — \
                         the gate lists its figures as advisory and moves on — and \
                         `--update` cannot add rows, so nothing but this assertion holds \
                         the scope in the file. Write the row."
                    )
                });
                assert_eq!(
                    got,
                    want,
                    "{cell}: tools/forge-budget.tsv says {got} for `{metric}` but \
                     measured::{} says {want}. One measurement, two files — ratchet BOTH \
                     in the same change.",
                    cell.to_uppercase().replace('-', "_"),
                );
            };
            require("third_party_packages", base.third_party as u64);
            require("third_party_loc", base.third_party_loc);
            require("build_scripts", base.build_scripts as u64);
            require("proc_macros", base.proc_macros as u64);
            require("duplicate_names", base.duplicate_names as u64);
        }
    }

    /// `vendor/forge.toml`'s `[forge] cells` block is a GENERATED record of
    /// [`crate::resolve::default_cells`] — `policy::seed_from_vendor` emits it
    /// and nothing reads it back for measurement. A judge proved the gap:
    /// replacing a ledger row with `{ name = "TOTALLY-BOGUS", triple =
    /// "sparc64-unknown-none", package = "does-not-exist" }` left `check`
    /// GREEN across "5 cell(s)" and this suite at 149/0, because the two were
    /// kept in sync only by an author's diligence. This is the comparison.
    #[test]
    fn the_ledger_header_and_default_cells_are_the_same_matrix() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(std::path::Path::parent)
            .expect("crates/aterm-forge sits two levels under the workspace root")
            .to_path_buf();
        let policy = crate::policy::load(&root).expect("vendor/forge.toml loads");
        let row = |c: &crate::model::Cell| (c.name.clone(), c.triple.clone(), c.package.clone());
        let ledger: Vec<_> = policy.forge.cells.iter().map(row).collect();
        let live: Vec<_> = crate::resolve::default_cells().iter().map(row).collect();
        assert_eq!(
            ledger, live,
            "vendor/forge.toml's [forge] cells has drifted from \
             resolve::default_cells(). The ledger block is generated FROM the \
             function — regenerate it (its own header says how) or fix \
             default_cells; a ledger row nothing measures reads as an audited \
             cell and is not one."
        );
    }
}
