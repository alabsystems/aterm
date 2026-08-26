// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Bundled-seed discovery — the OFFLINE batteries-included lane (§9.1, §11).
//!
//! A release cut may seal a signed seed registry (the flat `tools/atpkg-*.sh`
//! output: `index.toml`(`.sig`), `aterm-machines.toml`(`.sig`), `pkg-*.toml`
//! (`.sig`), artifact tarballs) into the app bundle at
//! `Contents/Resources/toolchain-seed`. This module resolves that directory
//! **co-located with the running executable only** — never via `PATH`, never
//! via config — mirroring the `aterm pkg` → `atpkg` co-location rule (§10), so
//! a directory somewhere else on the filesystem can never present itself as
//! this app's seed.
//!
//! Co-location is a SCOPING rule, not an authenticity one. This module does
//! not check the app's code signature, and must not be read as implying the
//! payload is trusted because it shipped inside a signed bundle — the payload
//! buys **zero trust** from where it sits. Its authenticity comes entirely
//! from its own chain: served through [`crate::DirFetcher`], the bytes pass
//! the identical verify-before-parse, freshness, floor, sha256 and `tree_root`
//! gates as any network registry, under the same pinned paper master. No
//! anchor, no installs.
//!
//! Resurrected (2026-08-17) from the lane deleted in `ba832933`, with one
//! deliberate tightening for the one-root model that landed in between: a
//! directory counts as a seed registry only when it holds the full signed
//! QUAD — the index pair AND the machine-roster pair — because `DirFetcher`
//! yields no candidate without the roster, so an index-pair-only dir could be
//! reported as a seed that can never verify (the label out-running the proof).
//!
//! `ATPKG_BUNDLED_SEED=<dir>` overrides for dev/tests (`ATPKG_BUNDLED_SEED=0`
//! disables the probe); the override is validated exactly like the co-located
//! probe — a directory that does not hold the signed quad resolves to nothing
//! rather than a half-registry.

use std::path::{Path, PathBuf};

/// The seed directory name under the bundle's `Resources/` (macOS) or beside
/// the executable (flat layouts). One name everywhere: the release cutter, the
/// Windows `build.ps1` sealing lane, the probe below, and the docs must agree
/// on this string.
///
/// # The `.lproj` suffix is LOAD-BEARING — do not "tidy" it away
///
/// codesign's built-in v2 resource rules contain
/// `^Resources/.*\.lproj/ = {optional: true, weight: 1000}`, so every file under
/// a `.lproj` directory is sealed with `optional = true`. Measured on macOS
/// 26.5.2 against real signed bundles: the payload present and unmodified
/// verifies, and the payload **entirely absent also verifies** (`codesign
/// --verify --deep --strict`, exit 0). Any other location fails — a plain
/// `Contents/Resources/<dir>` or `Contents/<dir>` breaks verification when
/// added OR removed, a bundle-root sibling makes codesign refuse to sign at
/// all, and custom `--resource-rules` omit rules produce a permanently invalid
/// signature on current macOS.
///
/// That one property is what lets a single signed, notarized bundle serve both
/// audiences: the DMG ships it WITH the payload (batteries included for a fresh
/// install) and the updater zip is the same bundle with this directory stripped
/// (~51 MB instead of ~800 MB, every update, forever). Rename this to anything
/// without the suffix and the lean zip stops verifying on every client.
///
/// The asymmetry to respect: you may sign fat and ship lean, NEVER sign lean and
/// add the payload later — an ADDED file under a `.lproj` is still a seal
/// violation. Modification is likewise refused; only whole-or-partial ABSENCE is
/// tolerated.
///
/// Known cosmetic effect: macOS bundle APIs enumerate `*.lproj` as
/// localizations, so this directory shows up in `Bundle.localizations` as a
/// pseudo-language named `toolchain-seed`. It can never be SELECTED (no user
/// locale matches it), and no alternative location has the optional-seal
/// property, so the pollution is accepted deliberately.
///
/// See `docs/GOLDEN-INSTALL-PATH.md` §4.
pub const SEED_DIR_NAME: &str = "toolchain-seed.lproj";

/// A directory only counts as a seed registry when the full signed QUAD is
/// present: the index pair plus the machine-roster pair. `index.toml` alone
/// (or an empty dir) is not a registry — [`crate::DirFetcher`] would yield no
/// candidate from it, and reporting one would let the status surface out-run
/// the proof.
fn holds_signed_registry(dir: &Path) -> bool {
    [
        "index.toml",
        "index.toml.sig",
        "aterm-machines.toml",
        "aterm-machines.toml.sig",
    ]
    .iter()
    .all(|f| dir.join(f).is_file())
}

/// Resolve the bundled seed registry, if this executable ships one.
///
/// Probe order:
/// 1. `ATPKG_BUNDLED_SEED` (dev/test override; `0`/`off`/empty disables the
///    whole probe — including co-located discovery, so a test run can assert
///    the no-seed path on a machine whose app bundle carries one);
/// 2. `<exe_dir>/../Resources/toolchain-seed` — the macOS `.app` layout, with
///    the executable at `Contents/MacOS/<bin>`;
/// 3. `<exe_dir>/toolchain-seed` — flat (dev tree, Windows dist —
///    `apps/aterm-win/build.ps1` seals exactly this layout).
///
/// The executable path is canonicalized first so the `atpkg`/`aterm-*` argv0
/// symlink aliases resolve to the real binary's home before the layout walk.
#[must_use]
pub fn bundled_seed_dir() -> Option<PathBuf> {
    match std::env::var("ATPKG_BUNDLED_SEED") {
        Ok(v) => {
            let v = v.trim();
            if v.is_empty() || v == "0" || v.eq_ignore_ascii_case("off") {
                return None;
            }
            let dir = PathBuf::from(v);
            return holds_signed_registry(&dir).then_some(dir);
        }
        Err(std::env::VarError::NotUnicode(_)) => return None,
        Err(std::env::VarError::NotPresent) => {}
    }
    let exe = std::env::current_exe().ok()?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    let exe_dir = exe.parent()?;
    co_located_candidates(exe_dir)
        .into_iter()
        .find(|d| holds_signed_registry(d))
}

/// The co-located probe candidates for an executable living in `exe_dir`, in
/// order. Split from [`bundled_seed_dir`] so the layout walk is unit-testable
/// without a real `.app` or process-global env.
#[must_use]
pub fn co_located_candidates(exe_dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::with_capacity(2);
    if let Some(contents) = exe_dir.parent() {
        out.push(contents.join("Resources").join(SEED_DIR_NAME));
    }
    out.push(exe_dir.join(SEED_DIR_NAME));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(label: &str) -> PathBuf {
        let d = std::env::temp_dir()
            .join("atpkg-bundled-tests")
            .join(format!("{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn candidates_prefer_the_app_bundle_resources_layout() {
        let exe_dir = Path::new("/Applications/aterm.app/Contents/MacOS");
        let c = co_located_candidates(exe_dir);
        assert_eq!(
            c[0],
            Path::new("/Applications/aterm.app/Contents/Resources").join(SEED_DIR_NAME)
        );
        assert_eq!(
            c[1],
            Path::new("/Applications/aterm.app/Contents/MacOS").join(SEED_DIR_NAME)
        );
    }

    /// The `.lproj` suffix is the entire reason the lean updater zip can strip
    /// this directory and still verify (see [`SEED_DIR_NAME`]). It is the kind of
    /// detail that looks like a typo to the next reader, so a rename goes RED
    /// here rather than silently shipping a bundle whose stripped zip fails
    /// `codesign --verify` on every client.
    #[test]
    fn the_seed_dir_name_keeps_its_load_bearing_lproj_suffix() {
        assert!(
            SEED_DIR_NAME.ends_with(".lproj"),
            "codesign seals `^Resources/.*\\.lproj/` as optional=true, and that is what \
             lets one signed bundle serve both the fat DMG and the stripped update zip; \
             SEED_DIR_NAME is currently {SEED_DIR_NAME:?}"
        );
        // Guard the OTHER half too: the optional rule is anchored at Resources/,
        // so a name containing a path separator would land somewhere the rule
        // does not match.
        assert!(
            !SEED_DIR_NAME.contains('/'),
            "the seed dir is one path component under Contents/Resources"
        );
    }

    #[test]
    fn a_dir_without_the_signed_quad_is_not_a_registry() {
        let d = scratch("no-quad");
        assert!(!holds_signed_registry(&d));
        std::fs::write(d.join("index.toml"), b"schema = 1").unwrap();
        // index.toml alone is NOT a registry — the sig must be present too.
        assert!(!holds_signed_registry(&d));
        std::fs::write(d.join("index.toml.sig"), [0u8; 64]).unwrap();
        // The index PAIR is still not enough under the one-root model: without
        // the roster pair, DirFetcher yields no candidate — not a registry.
        assert!(!holds_signed_registry(&d));
        std::fs::write(d.join("aterm-machines.toml"), b"roster_seq = 1").unwrap();
        assert!(!holds_signed_registry(&d));
        std::fs::write(d.join("aterm-machines.toml.sig"), [0u8; 64]).unwrap();
        assert!(holds_signed_registry(&d));
        let _ = std::fs::remove_dir_all(&d);
    }
}
