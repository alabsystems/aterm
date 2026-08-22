// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Security regression tests for #5573: page-backed Row use-after-free.
//!
//! ## Background
//!
//! Before #5573, `Row::new(cols, &mut PageStore)` and `Row::resize(new_cols, &mut PageStore)`
//! were safe public functions. This allowed safe code to create an owned page-backed row,
//! drop the backing `PageStore`, and then access dangling memory:
//!
//! ```text
//! // ORIGINAL BUG WITNESS (no longer compiles without `unsafe`):
//! let mut pages = PageStore::new();
//! let row = Row::new(8, &mut pages);   // was safe, now requires unsafe
//! drop(pages);                          // backing storage freed
//! let _ = row.as_slice();               // use-after-free!
//! ```
//!
//! The fix made `Row::new` and `Row::resize` `pub unsafe fn`, requiring callers
//! to explicitly opt into the lifetime invariant: the backing `PageStore` must
//! outlive all rows allocated from it.
//!
//! Additionally, `GridStorage.rows` was narrowed to `pub(crate)` to prevent
//! external code from extracting owned `Row` values that could outlive the
//! backing `PageStore`.
//!
//! ## What these tests verify
//!
//! 1. The correct `unsafe` usage pattern works (runtime check).
//! 2. Row operations after valid `unsafe` creation are sound (MIRI-exercisable).
//! 3. The `GridStorage.rows` field is not accessible from outside the crate
//!    (this is enforced by the compiler — `pub(crate)` is invisible to
//!    integration tests).
//!
//! ## What the compiler enforces (not testable at runtime)
//!
//! The following code would NOT compile, which is the security property:
//!
//! ```text
//! // COMPILE ERROR: call to unsafe function requires unsafe block
//! let mut pages = aterm_grid::PageStore::new();
//! let row = aterm_grid::Row::new(8, &mut pages);  // ERROR
//! ```
//!
//! ```text
//! // COMPILE ERROR: field `rows` of `GridStorage` is private
//! let grid_storage: aterm_grid::state::GridStorage = /* ... */;
//! let owned_row = grid_storage.rows.remove(0);  // ERROR
//! ```

use aterm_grid::{CellFlags, PackedColor, PageStore, Row};

// =========================================================================
// Correct unsafe usage: PageStore outlives rows
// =========================================================================

/// Verify that creating a row through the `unsafe` boundary works correctly
/// when the lifetime invariant is upheld.
///
/// Security invariant: `pages` must outlive `row`. Here both live in the
/// same scope, so `row` is dropped before `pages` (reverse declaration order).
#[test]
fn row_new_valid_unsafe_usage() {
    let mut pages = PageStore::new();
    // SAFETY: `pages` outlives `row` (same scope, reverse drop order).
    let row = unsafe { Row::new(80, &mut pages) };

    assert_eq!(row.cols(), 80);
    assert_eq!(row.len(), 0);
    assert!(row.is_empty());
}

/// Verify that resizing a row through the `unsafe` boundary works correctly.
///
/// Security invariant: `pages` must outlive `row` after the resize.
#[test]
fn row_resize_valid_unsafe_usage() {
    let mut pages = PageStore::new();
    // SAFETY: `pages` outlives `row` for the full scope.
    let mut row = unsafe { Row::new(40, &mut pages) };

    // Write content before resize
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;
    for col in 0..40u16 {
        row.write_char_styled(col, 'A', fg, bg, CellFlags::empty());
    }
    assert_eq!(row.len(), 40);

    // SAFETY: `pages` still outlives `row` after resize.
    unsafe { row.resize(80, &mut pages) };
    assert_eq!(row.cols(), 80);

    // Original content survived
    for col in 0..40u16 {
        assert_eq!(row.get(col).unwrap().char(), 'A', "col {col} content lost");
    }

    // New cells are empty
    for col in 40..80u16 {
        assert!(row.get(col).unwrap().is_empty(), "col {col} not empty");
    }
}

/// Verify that shrink-resize through the `unsafe` boundary works correctly.
///
/// Tests the inverse direction: large row resized to smaller.
#[test]
fn row_resize_shrink_valid_unsafe_usage() {
    let mut pages = PageStore::new();
    // SAFETY: `pages` outlives `row` for the full scope.
    let mut row = unsafe { Row::new(80, &mut pages) };

    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;
    for col in 0..80u16 {
        row.write_char_styled(col, 'B', fg, bg, CellFlags::empty());
    }

    // SAFETY: `pages` still outlives `row` after resize.
    unsafe { row.resize(40, &mut pages) };
    assert_eq!(row.cols(), 40);

    // Content within the new bounds survived
    for col in 0..40u16 {
        assert_eq!(row.get(col).unwrap().char(), 'B', "col {col} content lost");
    }
}

// =========================================================================
// Multiple rows sharing a PageStore: cross-row isolation
// =========================================================================

/// Allocate many rows from the same PageStore and verify they don't corrupt
/// each other. This exercises the page-backed allocation path that was the
/// root cause of the UAF when PageStore was dropped while rows were alive.
#[test]
fn multiple_rows_cross_isolation() {
    let mut pages = PageStore::new();
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    // SAFETY: All rows are dropped before `pages` (Vec dropped before local `pages`).
    let mut rows: Vec<Row> = (0..20)
        .map(|_| unsafe { Row::new(80, &mut pages) })
        .collect();

    // Write unique content to each row
    for (i, row) in rows.iter_mut().enumerate() {
        let c = char::from(b'A' + (i % 26) as u8);
        for col in 0..80u16 {
            row.write_char_styled(col, c, fg, bg, CellFlags::empty());
        }
    }

    // Verify each row's content is intact — no cross-row aliasing
    for (i, row) in rows.iter().enumerate() {
        let expected = char::from(b'A' + (i % 26) as u8);
        for col in 0..80u16 {
            assert_eq!(
                row.get(col).unwrap().char(),
                expected,
                "row[{i}][{col}] corrupted"
            );
        }
    }
}

/// Resize some rows while others remain unchanged. Verify that resizing one
/// row doesn't corrupt another row's backing storage.
#[test]
fn resize_one_row_doesnt_corrupt_others() {
    let mut pages = PageStore::new();
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    // SAFETY: All rows and `pages` live in the same scope.
    let mut row_a = unsafe { Row::new(40, &mut pages) };
    let mut row_b = unsafe { Row::new(40, &mut pages) };

    // Write distinct content
    for col in 0..40u16 {
        row_a.write_char_styled(col, 'X', fg, bg, CellFlags::empty());
        row_b.write_char_styled(col, 'Y', fg, bg, CellFlags::empty());
    }

    // Resize row_a — allocates new PageSlice, abandons old one
    // SAFETY: `pages` outlives both rows.
    unsafe { row_a.resize(120, &mut pages) };

    // row_b must be unaffected
    for col in 0..40u16 {
        assert_eq!(
            row_b.get(col).unwrap().char(),
            'Y',
            "row_b[{col}] corrupted after row_a resize"
        );
    }

    // row_a original content survived resize
    for col in 0..40u16 {
        assert_eq!(
            row_a.get(col).unwrap().char(),
            'X',
            "row_a[{col}] lost after resize"
        );
    }
}

// =========================================================================
// Compile-fail verification (against the REAL crate)
// =========================================================================

/// Locate the `deps/` directory holding this test binary — the same directory
/// Cargo puts `libaterm_grid-<hash>.rlib` and every transitive dependency in.
/// That is what lets the compile-fail probes below reference the REAL crate
/// instead of a synthetic stand-in.
fn deps_dir() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    // `target/<profile>/deps/<test>-<hash>`; some runners drop the test one level up.
    if dir.ends_with("deps") {
        Some(dir.to_path_buf())
    } else {
        Some(dir.join("deps"))
    }
}

/// The newest `libaterm_grid-*.rlib` in `deps/` (there may be several hashes
/// from older builds; the freshest matches this test binary).
fn aterm_grid_rlib() -> Option<std::path::PathBuf> {
    let dir = deps_dir()?;
    let mut best: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with("libaterm_grid-") && name.ends_with(".rlib") {
            let m = e.metadata().ok().and_then(|m| m.modified().ok())?;
            if best.as_ref().is_none_or(|(t, _)| m > *t) {
                best = Some((m, e.path()));
            }
        }
    }
    best.map(|(_, p)| p)
}

/// Resolve the probe's compiler — BY EVIDENCE, not by guess.
///
/// The one property that matters is agreement with the rlib: the probe must
/// use a compiler that can CONSUME the `aterm_grid` metadata this very test
/// binary was built against, or every result is about toolchain skew instead
/// of aterm-grid. This used to be assumed positionally — "the stage2 dir
/// first, PATH `rustc` second" — which was right exactly as long as the
/// workspace was built by that stage2. The first machine that built with the
/// atpkg-installed toolchain instead (PATH `trustc`, no rustup) broke the
/// assumption: the probe found the six-weeks-stale dev stage2, E0514'd on the
/// fresh rlib, and the positive control failed on a healthy tree.
///
/// So the DEFAULT candidates — PATH `trustc` (the atpkg lane; also the rustup
/// shim on a dev box, which resolves to stage2 anyway), the conventional
/// `$HOME/trust/build/host/stage2/bin` dev checkout (canonicalized — protected
/// Trust drivers refuse symlinked toolchain paths), then PATH `rustc`
/// (upstream boxes) — are each VETTED with a metadata-touch compile against
/// the real rlib, and the first that passes wins. E0514 fires at metadata
/// load, so the vet is precisely the skew check. If NONE vets, the first that
/// merely runs is returned so the positive control can fail with the real
/// compiler stderr instead of a bare "no compiler".
///
/// `TRUST_STAGE2_BIN` is different: an explicit operator override names THE
/// compiler, so it is honored without vetting — silently falling back from an
/// explicit choice would make the probe disagree with what the operator asked
/// for (the same fail-closed rule Trust applies to `AY_PATH`). Skew under an
/// override still surfaces loudly, through the positive control's diagnosis.
///
/// The bool is "this is trustc" — the caller adds the verification off-switch
/// (a direct compiler invocation bypasses .cargo/config.toml's native-lane
/// opt-out, and an unverified probe snippet is the point here).
fn probe_compiler(
    rlib: &std::path::Path,
    deps: &std::path::Path,
) -> Option<(std::path::PathBuf, bool)> {
    let runs = |path: &std::path::Path| {
        std::process::Command::new(path)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    };
    if let Some(dir) = std::env::var_os("TRUST_STAGE2_BIN") {
        let explicit = std::fs::canonicalize(std::path::PathBuf::from(dir))
            .map(|physical| physical.join("trustc"))
            .ok()?;
        return runs(&explicit).then_some((explicit, true));
    }
    let mut candidates: Vec<(std::path::PathBuf, bool)> = Vec::new();
    candidates.push((std::path::PathBuf::from("trustc"), true));
    if let Some(home) = std::env::var_os("HOME")
        && let Ok(physical) =
            std::fs::canonicalize(std::path::Path::new(&home).join("trust/build/host/stage2/bin"))
    {
        candidates.push((physical.join("trustc"), true));
    }
    candidates.push((std::path::PathBuf::from("rustc"), false));
    candidates.retain(|(path, _)| runs(path));
    // The vet: `extern crate` alone forces the metadata load where an
    // incompatible-compiler rlib is rejected, and asserts nothing about the
    // crate's API — a vet that used a real snippet could never be told apart
    // from the probes it exists to make meaningful.
    let fallback = candidates.first().cloned();
    candidates
        .into_iter()
        .find(|(path, is_trustc)| {
            compile_probe(path, *is_trustc, rlib, deps, "extern crate aterm_grid;")
                .is_some_and(|out| out.status.success())
        })
        .or(fallback)
}

/// Compile `src` against the real `aterm_grid` rlib. `Some(true)` = compiled,
/// `Some(false)` = rejected, `None` = could not run the probe at all (no
/// usable compiler, no rlib) — reported as a SKIP rather than a silent pass.
/// The verification off-switch spelling THIS compiler accepts.
///
/// Hardcoding one spelling silently voids this whole file. The two spellings
/// partition the compilers (AGENTS.md "Flag-spelling skew"): a `trustc` that
/// does not know the one we pass rejects it at flag-parse, so the probe fails to
/// build a VALID reference, the harness declares itself broken, and every
/// compile-fail assertion below it proves nothing — on a SECURITY regression
/// suite. That is exactly what was happening here: the literal
/// `-Ztrust-verify=off` is rejected by every trust compiler on this machine.
///
/// So ask the compiler instead of assuming. `-Z help` lists the options it
/// actually has; prefer whichever off-switch appears there, and fall back to the
/// post-rename spelling when the probe cannot be read (an unusable compiler is
/// already reported as a SKIP downstream, never a silent pass).
fn trust_off_switch(compiler: &std::path::Path) -> &'static str {
    let help = std::process::Command::new(compiler)
        .arg("-Zhelp")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    if help.contains("no-trust-verify") {
        "-Zno-trust-verify"
    } else {
        "-Ztrust-verify=off"
    }
}

/// One compile of `src` against the rlib under the given compiler — the ONE
/// command shape shared by the candidate vet and every real probe, so the
/// compiler that passed the vet is exercised in exactly the way it was vetted.
fn compile_probe(
    compiler: &std::path::Path,
    is_trustc: bool,
    rlib: &std::path::Path,
    deps: &std::path::Path,
    src: &str,
) -> Option<std::process::Output> {
    let out = std::env::temp_dir().join(format!(
        "aterm_grid_probe_{}_{:x}",
        std::process::id(),
        // Distinct output per snippet: the vet and a probe may run back to
        // back, and a shared path would let a stale artifact mask a failure.
        src.len() ^ src.as_bytes().iter().map(|&b| b as usize).sum::<usize>()
    ));
    let mut cmd = std::process::Command::new(compiler);
    if is_trustc {
        cmd.arg(trust_off_switch(compiler));
    }
    let mut child = cmd
        .arg("--edition=2021")
        .arg("--crate-type=lib")
        .arg("--extern")
        .arg(format!("aterm_grid={}", rlib.display()))
        .arg("-L")
        .arg(format!("dependency={}", deps.display()))
        .arg("-o")
        .arg(&out)
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    {
        use std::io::Write;
        child.stdin.as_mut()?.write_all(src.as_bytes()).ok()?;
    }
    let probe_out = child.wait_with_output().ok()?;
    let _ = std::fs::remove_file(&out);
    Some(probe_out)
}

fn probe_compiles(src: &str) -> Option<bool> {
    let rlib = aterm_grid_rlib()?;
    let deps = deps_dir()?;
    let (compiler, is_trustc) = probe_compiler(&rlib, &deps)?;
    let probe_out = compile_probe(&compiler, is_trustc, &rlib, &deps, src)?;
    if !probe_out.status.success() {
        // KEPT, not discarded. A probe that fails for an environmental reason
        // (a compiler that rejects the off-switch spelling, an rlib built by a
        // different rustc) previously failed with a generic "the harness is
        // broken", and the one line that said WHY was thrown away — so the
        // positive control could not distinguish "aterm-grid regressed" from
        // "this machine's toolchain is mixed". On a security suite that is the
        // difference between a finding and a wild goose chase.
        *last_probe_stderr().lock().expect("probe stderr mutex") =
            String::from_utf8_lossy(&probe_out.stderr).into_owned();
    }
    Some(probe_out.status.success())
}

/// The last failing probe's compiler stderr, so the positive control can quote
/// it instead of guessing.
fn last_probe_stderr() -> &'static std::sync::Mutex<String> {
    static S: std::sync::OnceLock<std::sync::Mutex<String>> = std::sync::OnceLock::new();
    S.get_or_init(|| std::sync::Mutex::new(String::new()))
}

/// POSITIVE CONTROL: the probe harness itself works — a coercion to an
/// `unsafe fn` pointer of the real signature MUST compile. Without this, a
/// broken harness (wrong rlib path, missing dep) would make every negative
/// probe below "pass" for the wrong reason. That is exactly how the previous
/// version of these tests went vacuous.
#[test]
fn compile_probe_harness_actually_reaches_aterm_grid() {
    let src = "pub fn t() { let _: unsafe fn(u16, &mut aterm_grid::PageStore) -> aterm_grid::Row \
               = aterm_grid::Row::new; }";
    match probe_compiles(src) {
        Some(true) => {}
        Some(false) => {
            let why = last_probe_stderr()
                .lock()
                .expect("probe stderr mutex")
                .clone();
            // Name the environmental causes explicitly: both are toolchain skew
            // on this machine, not a regression in aterm-grid, and both have a
            // remedy that has nothing to do with this crate.
            let hint = if why.contains("incompatible version of rustc") {
                "\nLIKELY CAUSE: the aterm_grid rlib was built by a DIFFERENT rustc than the \
                 probe compiler (a mixed-compiler build — e.g. RUSTC= overridden to dodge an \
                 ICE). Build the crate and run the probe with the same toolchain."
            } else if why.contains("unknown unstable option") {
                "\nLIKELY CAUSE: the probe compiler rejects the verification off-switch \
                 spelling — see AGENTS.md \"Flag-spelling skew\". `trust_off_switch` asks the \
                 compiler which one it knows, so this means it answered with neither."
            } else {
                ""
            };
            panic!(
                "the compile probe could not build a VALID reference to aterm_grid::Row::new — \
                 the harness is broken, so the compile-fail tests below prove nothing.{hint}\n\
                 --- probe compiler stderr ---\n{why}"
            )
        }
        None => eprintln!("SKIP: no rustc / no aterm_grid rlib for the compile probe"),
    }
}

/// `Row::new` must not be callable from safe code.
///
/// REGRESSION: this test used to pipe a SYNTHETIC snippet to `rustc`
/// (`unsafe fn create() -> u8 { 0 } fn main() { let _ = create(); }`) that never
/// mentioned `Row` at all — it asserted that rustc rejects safe calls to unsafe
/// fns, i.e. it tested the COMPILER, not aterm-grid. Making `Row::new` safe
/// again — the #5573 use-after-free — would not have failed it.
///
/// A coercion to a SAFE fn pointer is the real assertion: it compiles if and
/// only if `Row::new` is safe, and needs no constructible `PageStore`.
#[test]
fn row_new_rejects_safe_call() {
    let src = "pub fn t() { let _: fn(u16, &mut aterm_grid::PageStore) -> aterm_grid::Row \
               = aterm_grid::Row::new; }";
    match probe_compiles(src) {
        Some(false) => {}
        Some(true) => panic!(
            "aterm_grid::Row::new coerced to a SAFE fn pointer — it is no longer \
             `unsafe fn`, reopening the #5573 page-backed use-after-free"
        ),
        None => eprintln!("SKIP: no rustc / no aterm_grid rlib for the compile-fail check"),
    }
}

/// `Row::resize` must not be callable from safe code. Same reasoning as
/// [`row_new_rejects_safe_call`]; this one also used a synthetic snippet.
#[test]
fn row_resize_rejects_safe_call() {
    let src = "pub fn t() { let _: fn(&mut aterm_grid::Row, u16, &mut aterm_grid::PageStore) \
               = aterm_grid::Row::resize; }";
    match probe_compiles(src) {
        Some(false) => {}
        Some(true) => panic!(
            "aterm_grid::Row::resize coerced to a SAFE fn pointer — it is no longer \
             `unsafe fn`, reopening the #5573 page-backed use-after-free"
        ),
        None => eprintln!("SKIP: no rustc / no aterm_grid rlib for the compile-fail check"),
    }
}
