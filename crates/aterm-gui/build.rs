// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// build.rs — stamp build provenance into the binary as compile-time env vars,
// surfaced at runtime by `src/build_info.rs` (the About panel + `aterm-ctl version`).
//
//   ATERM_GIT_COMMIT   short commit the binary was built from (+ "-dirty" when the
//                      working tree had uncommitted changes); "unknown" w/o git.
//   ATERM_BUILD_NUMBER monotonic build number (the version is a plain MAJOR.MINOR;
//                      ordering lives in this metadata): SOURCE_DATE_EPOCH — the
//                      ledger claim `cargo ship cut` exports — wins when set; else
//                      HEAD's committer Unix epoch (dev builds); "0" only w/o git.
//   ATERM_BUILD_TIME   UTC build timestamp (RFC3339), or "unknown".
//
// Plus COMPILER provenance (which rustc produced this binary — matters because this
// box carries both upstream Rust and the Trust fork, and different machines carry
// DIFFERENT trust builds), parsed from `$RUSTC -vV` by src/compiler_probe.rs
// (include!d below so the same parser is unit-tested under `cargo test`):
//
//   ATERM_RUSTC_VERSION  full first line of `$RUSTC -vV`
//   ATERM_RUSTC_COMMIT   the compiler's full git commit hash
//   ATERM_RUSTC_HOST     the compiler's host triple
//   ATERM_COMPILER_FLAVOR 'r' (upstream Rust) | 't' (Trust fork)
//   ATERM_BUILD_PROFILE  cargo PROFILE ("debug"/"release")
//   ATERM_TRUST_VERIFY   "on" iff --cfg trust_verify was active, else "off"
//
// All probes are best-effort: a missing `git`/`date` degrades to "unknown" rather
// than failing the build (so a source tarball without a .git still compiles).

use base64::Engine as _;
use sha2::{Digest, Sha256};
use std::process::Command;

/// Fixed 64-byte lowercase record used only by unpinned development builds.
/// A release always supplies a valid `ATERM_UPDATE_PUBKEY`, so the release
/// cutter compares the Mach-O record against the permanent authority and
/// rejects this sentinel.
const UNPINNED_UPDATE_PIN_SENTINEL: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

// Shared with the test suite: `main.rs` mounts the same file as a #[cfg(test)]
// module, so the -vV parser and the flavor classifier are tested against real
// fixtures while build.rs uses the identical code.
include!("src/compiler_probe.rs");

fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn main() {
    // Derive the updater-key fingerprint from the SAME compile-time input that
    // `aterm-update::PINNED_UPDATE_PUBKEY` consumes.  `build_info.rs` places this
    // exact fixed-width value in `__DATA,__aterm_upin`, allowing the release
    // cutter to prove the x86_64 slice's authority without executing it under
    // Rosetta.  Invalid non-empty inputs fail the build instead of embedding an
    // ambiguous record; an ordinary unpinned dev build gets the explicit zero
    // sentinel and remains updater-inert.
    let update_pin_sha256 = match std::env::var("ATERM_UPDATE_PUBKEY") {
        Ok(encoded) if !encoded.is_empty() => {
            let raw = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .expect("ATERM_UPDATE_PUBKEY must be standard base64");
            assert_eq!(
                raw.len(),
                32,
                "ATERM_UPDATE_PUBKEY must decode to an Ed25519 32-byte public key"
            );
            Sha256::digest(raw)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        }
        _ => UNPINNED_UPDATE_PIN_SENTINEL.to_string(),
    };
    println!("cargo:rustc-env=ATERM_UPDATE_PIN_SHA256={update_pin_sha256}");
    println!("cargo:rerun-if-env-changed=ATERM_UPDATE_PUBKEY");

    // Git commit (short, 12 hex) + a "-dirty" suffix when the tree isn't clean.
    let commit =
        run("git", &["rev-parse", "--short=12", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dirty = run("git", &["status", "--porcelain"]).is_some_and(|s| !s.is_empty());
    let commit = if commit != "unknown" && dirty {
        format!("{commit}-dirty")
    } else {
        commit
    };
    println!("cargo:rustc-env=ATERM_GIT_COMMIT={commit}");

    // Monotonic build number, epoch-scale (seconds). The version is a plain
    // MAJOR.MINOR (see `build_info::VERSION`); the monotonic ordering the updater's
    // "apply only if greater" gate needs is metadata. A dev build derives it from
    // HEAD's committer Unix epoch:
    //   * strictly monotonic across releases — a later release is a later commit;
    //   * STABLE across rebuilds of the same commit — the committer date is frozen in the
    //     commit object, so two builds of one release agree.
    //   * topology-INDEPENDENT — a per-commit field, so it survives a shallow clone and
    //     rebase/squash/amend just re-stamp it forward; no hand-bumped counter to drift.
    // Fits a single sub-2^32 CFBundleVersion component (~1.78e9 today, valid past 2100).
    //
    // COORDINATION: `SOURCE_DATE_EPOCH`, when a valid epoch, WINS over the live `git`
    // read — it is the release PIN. `cargo ship cut` (crates/aterm-release) claims the
    // build number from the append-only `RELEASES.ledger` (`n = max(tail+1, unix_now)`,
    // the same epoch scale as the dev fallback) and exports it ONCE, so this binary's
    // `ATERM_BUILD_NUMBER`, the bundle's sealed CFBundleVersion, and the release
    // manifest's `build_number` all carry the SAME n even if HEAD moves between build
    // steps — the tool's post-build self-check asserts the triple agreement. Without
    // the pin they would each re-read HEAD at a different instant and could DISAGREE,
    // which fails the updater's sealed-CFBundleVersion == manifest.build_number
    // anti-replay bind. Unset (a dev build) ⇒ HEAD's committer epoch; "0" only w/o git.
    let is_epoch = |s: &String| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let build_number = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .filter(is_epoch)
        .or_else(|| run("git", &["show", "-s", "--format=%ct", "HEAD"]).filter(is_epoch))
        .unwrap_or_else(|| "0".into());
    println!("cargo:rustc-env=ATERM_BUILD_NUMBER={build_number}");

    // Build timestamp (UTC, RFC3339). Honour SOURCE_DATE_EPOCH for reproducible
    // builds when set; otherwise stamp the current wall clock.
    let build_time = match std::env::var("SOURCE_DATE_EPOCH") {
        Ok(epoch) if !epoch.is_empty() => run("date", &["-u", "-r", &epoch, "+%Y-%m-%dT%H:%M:%SZ"]),
        _ => run("date", &["-u", "+%Y-%m-%dT%H:%M:%SZ"]),
    }
    .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=ATERM_BUILD_TIME={build_time}");

    // Compiler provenance: interrogate the ACTUAL compiler cargo is about to use
    // (cargo sets RUSTC for build scripts; bare "rustc" is the no-cargo fallback)
    // rather than trusting whatever is first on PATH — per-binary provenance is the
    // whole point when upstream Rust and the Trust fork coexist on one machine.
    let rustc_path = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let vv = run(&rustc_path, &["-vV"]).unwrap_or_default();
    let rustc = parse_rustc_vv(&vv);
    // Flavor ('r' = upstream Rust, 't' = Trust fork), in priority order:
    //   1. explicit ATERM_COMPILER_FLAVOR env override ('r'|'t'; junk ignored);
    //   2. the -vV self-identification ('binary: trustc' / '(trustc)' version line
    //      — the 2026-07 toolchains stamp both; covers the ATERM_CARGO=targo lane,
    //      where a bare PATH-resolved `rustc` sets no env hint at all);
    //   3. RUSTC path containing '/trust/' (the fork lives in $HOME/trust/build/...);
    //   4. RUSTUP_TOOLCHAIN == 'trust' (a linked `rustup run trust` lane);
    //   5. default 'r'.
    // NOT inferred from '-dev' in the release — any local rustc build reports -dev.
    let explicit = std::env::var("ATERM_COMPILER_FLAVOR").ok();
    let toolchain = std::env::var("RUSTUP_TOOLCHAIN").ok();
    let flavor = detect_flavor(explicit.as_deref(), &vv, &rustc_path, toolchain.as_deref());
    println!("cargo:rustc-env=ATERM_RUSTC_VERSION={}", rustc.version_line);
    println!("cargo:rustc-env=ATERM_RUSTC_COMMIT={}", rustc.commit);
    println!("cargo:rustc-env=ATERM_RUSTC_HOST={}", rustc.host);
    println!("cargo:rustc-env=ATERM_COMPILER_FLAVOR={flavor}");
    println!(
        "cargo:rustc-env=ATERM_BUILD_PROFILE={}",
        std::env::var("PROFILE").unwrap_or_else(|_| "unknown".into())
    );
    // "on" iff this build compiled with `--cfg trust_verify` (cargo exposes active
    // cfgs to build scripts as CARGO_CFG_*), so About/ctl can say whether the
    // Trust-verify hardening was in the compile.
    println!(
        "cargo:rustc-env=ATERM_TRUST_VERIFY={}",
        if std::env::var_os("CARGO_CFG_TRUST_VERIFY").is_some() {
            "on"
        } else {
            "off"
        }
    );
    println!("cargo:rerun-if-env-changed=RUSTC");
    println!("cargo:rerun-if-env-changed=RUSTUP_TOOLCHAIN");
    println!("cargo:rerun-if-env-changed=ATERM_COMPILER_FLAVOR");

    // Re-stamp when HEAD moves (new commit / checkout) or the version changes (the
    // workspace `[workspace.package] version` in the root Cargo.toml drives the build
    // number). The workspace `.git` + Cargo.toml are two levels up from this manifest.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/index");
    println!("cargo:rerun-if-changed=../../Cargo.toml");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");

    // Windows: compile + link the application icon into the exe so the taskbar
    // button, Alt-Tab, titlebar and Explorer show the aterm icon instead of the
    // generic exe glyph. Gated on the TARGET os (build scripts run on the host,
    // so `cfg!` would reflect the wrong platform) and `manifest_optional()` keeps
    // a toolchain-less build working — it downgrades a missing resource compiler
    // to a warning rather than failing. No-op on every non-Windows target.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=assets/aterm.rc");
        println!("cargo:rerun-if-changed=assets/aterm.ico");
        if let Err(e) =
            embed_resource::compile("assets/aterm.rc", embed_resource::NONE).manifest_optional()
        {
            println!("cargo:warning=aterm-gui: window icon not embedded: {e}");
        }
    }
}
