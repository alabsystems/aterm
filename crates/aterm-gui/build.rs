// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// build.rs — stamp the build's STABLE provenance into this crate as compile-time
// env vars, surfaced at runtime by `src/build_info.rs` (the About panel +
// `aterm-ctl version`).
//
// NOT HERE: the per-commit stamp (commit, dirty flag, build number, build time,
// dev counter). `crates/aterm/build.rs` stamps it into the binary crate, and the
// binary hands it to this library at the top of `main` (`build_info::BuildStamp`).
// Everything a build script emits reaches every unit of its package, so while this
// script stamped the commit, every commit (an empty one included) recompiled all
// of aterm-gui and everything linking it. What this script emits now changes only
// with the compiler, the profile, the flags or the committed update pin.
//
// Plus COMPILER provenance (which compiler produced this binary — matters because
// this box carries both upstream Rust and the Trust toolchain's trustc, and
// different machines carry DIFFERENT trust builds), parsed from `$RUSTC -vV` by
// src/compiler_probe.rs (include!d below so the same parser is unit-tested under
// the test suite; RUSTC is the env name cargo AND targo hand build scripts):
//
//   ATERM_COMPILER_VERSION_LINE  full first line of `$RUSTC -vV`
//   ATERM_COMPILER_COMMIT        the compiler's full git commit hash
//   ATERM_COMPILER_HOST          the compiler's host triple
//   ATERM_COMPILER_TRUST_VERSION Trust's OWN version (the `trust:` -vV line), "" if none
//   ATERM_COMPILER_FLAVOR 'r' (upstream Rust) | 't' (Trust: trustc)
//   ATERM_BUILD_PROFILE  cargo PROFILE ("debug"/"release")
//   ATERM_TRUST_VERIFY   "on" iff --cfg trust_verify was active, else "off"
//
// All probes are best-effort: a missing compiler answer degrades to "unknown"
// rather than failing the build.

use aterm_digest::Sha256;
use std::process::Command;

/// Fixed 64-byte lowercase record used only when no paper master is pinned (a fork).
/// This tree always has one (`aterm_update_core::pins::PAPER_MASTER_PUBKEYS`), so the
/// release cutter compares the Mach-O record against it and rejects this sentinel.
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
    // ONE NAME FOR "the accessibility tree is compiled in". It is a FEATURE on
    // macOS/Windows (where the platform has its own surface, or the weight is
    // genuinely optional) and a PLATFORM FACT on Linux, where AccessKit is the
    // only path to AT-SPI and therefore to a screen reader. Folding both here
    // keeps 70-odd cfg sites from having to spell the policy — and keeps the
    // policy in one place when it changes again.
    println!("cargo::rustc-check-cfg=cfg(a11y_tree)");
    let a11y_feature = std::env::var_os("CARGO_FEATURE_A11Y_ACCESSKIT").is_some();
    let linux = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux");
    if a11y_feature || linux {
        println!("cargo::rustc-cfg=a11y_tree");
    }

    // The update pin: the fingerprint of the paper master, the ONE anchor that
    // authorizes a release — the SAME compile-time input `aterm-update`'s
    // `compiled_update_pin_sha256` reports. `build_info.rs` places this exact
    // fixed-width value in `__DATA,__aterm_upin`, allowing the release cutter to prove
    // the x86_64 slice's authority without executing it under Rosetta. Invalid
    // non-empty inputs fail the build instead of embedding an ambiguous record; a fork
    // with no master gets the explicit zero sentinel.
    // The anchor is the COMMITTED constant, not build-environment state: read it from
    // the one file that owns it so the embedded record proves the same key the runtime
    // verifies under.
    let update_pin_sha256 = match aterm_update_core::pins::PAPER_MASTER_PUBKEYS.first() {
        Some(encoded) if !encoded.is_empty() => {
            let raw = aterm_codec::base64::decode_strict(encoded.as_bytes())
                .expect("PAPER_MASTER_PUBKEYS[0] must be standard base64");
            assert_eq!(
                raw.len(),
                32,
                "PAPER_MASTER_PUBKEYS[0] must decode to an Ed25519 32-byte public key"
            );
            aterm_codec::hex::encode_lower(&Sha256::digest(raw))
        }
        _ => UNPINNED_UPDATE_PIN_SENTINEL.to_string(),
    };
    println!("cargo:rustc-env=ATERM_UPDATE_PIN_SHA256={update_pin_sha256}");

    println!(
        "cargo:rustc-env=ATERM_BINARY_TARGET={}",
        std::env::var("TARGET").expect("Cargo target")
    );

    // Compiler provenance: interrogate the ACTUAL compiler the build driver is
    // about to use (cargo and targo both set RUSTC for build scripts — targo's
    // custom_build lane keeps the env name for drop-in compatibility; bare
    // "rustc" is the no-driver fallback) rather than trusting whatever is first
    // on PATH — per-binary provenance is the whole point when upstream Rust and
    // the Trust toolchain (trustc) coexist on one machine.
    let compiler_path = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let vv = run(&compiler_path, &["-vV"]).unwrap_or_default();
    let compiler = parse_rustc_vv(&vv);
    // Flavor ('r' = upstream Rust, 't' = Trust fork), in priority order:
    //   1. the -vV self-identification ('binary: trustc' / '(trustc)' version line
    //      — the 2026-07 toolchains stamp both; covers the lane where a bare
    //      PATH-resolved `rustc` sets no env hint at all);
    //   2. RUSTC path containing '/trust/' (the fork lives in $HOME/trust/build/...);
    //   3. RUSTUP_TOOLCHAIN == 'trust' (a linked `rustup run trust` lane);
    //   4. default 'r'.
    // NOT inferred from '-dev' in the release — any local rustc build reports -dev.
    // There is NO env override: an ATERM_COMPILER_FLAVOR that outranked the
    // compiler's own self-report let a shell claim trustc provenance for a
    // binary upstream rustc built, in the one field About and `ctl version` ship
    // AS evidence. The three signals above cover every lane it was added for.
    let toolchain = std::env::var("RUSTUP_TOOLCHAIN").ok();
    let flavor = detect_flavor(&vv, &compiler_path, toolchain.as_deref());
    println!(
        "cargo:rustc-env=ATERM_COMPILER_VERSION_LINE={}",
        compiler.version_line
    );
    println!("cargo:rustc-env=ATERM_COMPILER_COMMIT={}", compiler.commit);
    println!("cargo:rustc-env=ATERM_COMPILER_HOST={}", compiler.host);
    // Trust's OWN version (the `trust:` -vV line, e.g. 0.1.0) — "" when the
    // compiler reports none. Distinct from the rustc-shaped release token in the
    // version line, which is the Rust release Trust is compatible with.
    println!(
        "cargo:rustc-env=ATERM_COMPILER_TRUST_VERSION={}",
        compiler.trust_version
    );
    println!("cargo:rustc-env=ATERM_COMPILER_FLAVOR={flavor}");
    println!(
        "cargo:rustc-env=ATERM_BUILD_PROFILE={}",
        std::env::var("PROFILE").unwrap_or_else(|_| "unknown".into())
    );
    // Whether the Trust verification pipeline was really in this compile, for
    // About and `aterm ctl version`. See `trust_verify_state` — the `--print cfg`
    // probe alone answers this WRONG on a Trust toolchain.
    println!(
        "cargo:rustc-env=ATERM_TRUST_VERIFY={}",
        trust_verify_state()
    );
    println!("cargo:rerun-if-env-changed=CARGO_ENCODED_RUSTFLAGS");
    println!("cargo:rerun-if-env-changed=RUSTC");
    println!("cargo:rerun-if-env-changed=RUSTUP_TOOLCHAIN");

    // No `rerun-if-changed` on git or on the workspace manifest: nothing above
    // reads either (the per-commit stamp lives in `crates/aterm/build.rs`). The
    // update pin comes from a build-dependency, which cargo already tracks.
    println!("cargo:rerun-if-changed=build.rs");

    // Windows: compile + link the application icon into the DEV `aterm-gui.exe`
    // so the taskbar button, Alt-Tab, titlebar and Explorer show the aterm icon
    // instead of the generic exe glyph (the shipped `aterm.exe` gets its
    // resources from crates/aterm/build.rs — link args reach only the emitting
    // package's own bins). Gated on the TARGET os (build scripts run on the
    // host, so `cfg!` would reflect the wrong platform); a missing resource
    // compiler is a warning rather than a failure, so a toolchain-less build
    // still works. No-op on every non-Windows target.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=assets/aterm.ico");
        let outcome = aterm_winres::compile(std::path::Path::new("assets/aterm.rc"));
        if !outcome.is_linked() {
            println!("cargo:warning=aterm-gui: window icon not embedded: {outcome}");
        }
    }
}

/// `"on"` iff this compile really runs the Trust verification pipeline.
///
/// `CARGO_CFG_TRUST_VERIFY` alone is NOT the answer, and trusting it shipped a
/// false provenance line for months. Cargo derives every `CARGO_CFG_*` from a
/// `rustc --print cfg` probe, and targo deliberately strips the `-Ztrust-verify`
/// family from that probe — so on a Trust toolchain the cfg reports the
/// compiler's DEFAULT (verification on) while every real unit is compiled with
/// `-Ztrust-verify=off` by the `targo --unverified` lane (until 2026-10-07 by
/// `.cargo/config.toml`, which carries none now). Printing that as "on" claims
/// a hardening the binary does not carry, which is exactly what the honesty
/// ratchet forbids. The lane's switch reaches this script in
/// `CARGO_ENCODED_RUSTFLAGS` (measured 2026-10-07 on seal 321aaeda7, for
/// `targo --unverified` and for a compat `cargo` given the cfg-scoped
/// `--config` switch alike).
///
/// So the rustflags cargo will actually pass to rustc win over the probe, and
/// the probe is only the fallback for the case it does answer correctly: no
/// explicit flag at all (a stock-Rust build, where the cfg is simply unset).
///
/// Both spellings of the off-switch are recognised — `-Ztrust-verify=off` is
/// current, `-Zno-trust-verify=yes` is the retired one — and both the joined
/// (`-Ztrust-verify=off`) and split (`-Z` `trust-verify=off`) argument forms,
/// since a rustflags list may carry either.
fn trust_verify_state() -> &'static str {
    fn from_value(flag: &str) -> Option<&'static str> {
        if let Some(v) = flag.strip_prefix("trust-verify=") {
            return Some(if v.eq_ignore_ascii_case("off") {
                "off"
            } else {
                "on"
            });
        }
        if let Some(v) = flag.strip_prefix("no-trust-verify=") {
            return Some(if v.eq_ignore_ascii_case("yes") {
                "off"
            } else {
                "on"
            });
        }
        None
    }

    if let Some(encoded) = std::env::var_os("CARGO_ENCODED_RUSTFLAGS") {
        let encoded = encoded.to_string_lossy().into_owned();
        // CARGO_ENCODED_RUSTFLAGS is 0x1f-separated (cargo's documented encoding).
        let flags: Vec<&str> = encoded
            .split('\u{1f}')
            .filter(|flag| !flag.is_empty())
            .collect();
        let mut expect_z_value = false;
        for flag in flags {
            if expect_z_value {
                expect_z_value = false;
                if let Some(state) = from_value(flag) {
                    return state;
                }
                continue;
            }
            if flag == "-Z" {
                expect_z_value = true;
                continue;
            }
            if let Some(rest) = flag.strip_prefix("-Z")
                && let Some(state) = from_value(rest)
            {
                return state;
            }
        }
    }

    // No explicit flag: the cfg probe is authoritative (and on stock Rust, unset).
    if std::env::var_os("CARGO_CFG_TRUST_VERIFY").is_some() {
        "on"
    } else {
        "off"
    }
}
