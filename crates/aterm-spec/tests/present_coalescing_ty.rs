// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Proof-Carrying Performance: the input-to-photon latency fix (v0.5.2).
//!
//! The default-on cursor aurora re-presents for ~260ms after every cursor move. In the
//! OLD code EVERY present — including an aurora animation tick — stamped the frame-pacing
//! clock (`last_present_at`), so the `Wake::Output` soft cap saw a "recent present" and
//! DEFERRED an echo typed during the aurora tail. The deferred-flush still fired it
//! within ONE frame interval (NOT an indefinite stall), but it lost up to that frame
//! interval (~8ms on a 120Hz ProMotion panel — the M-series default; ~16ms on 60Hz) of
//! input-to-photon time it should not have. The fix stamps the clock only on genuine
//! CONTENT presents (`content_pending`), so an aurora tick never defers an echo and a
//! content-idle echo presents immediately (`main.rs`, `app_render.rs`).
//!
//! `crates/aterm-spec-models/specs/PresentCoalescing.tla` models that state machine with
//! BOTH clocks (`lastContent` = content-only = the fix; `lastAny` = all presents = the
//! old code) AND the deferred-flush, so the catch is FAITHFUL: the old latency was finite
//! (the deferred-flush bounded it to one frame), and the win is removing the
//! up-to-one-frame aurora-induced deferral, not an indefinite stall. The EFFICIENCY
//! THEOREM `ContentIdleEchoImmediate` — an echo that ARRIVES when the last genuine
//! content present is already a frame interval old (no real output burst to coalesce) is
//! presented at its arrival instant, never deferred by an aurora tick (legitimate
//! sub-frame coalescing of an ACTUAL content burst is NOT claimed away) — is discharged
//! by the same `ty` BFS that gates the committed specs, under the Buggy-constant
//! prove-and-catch convention (`tests/rfc_worked_examples_ty.rs`):
//!
//!   * `Buggy = 0` (the fix, cap reads the content clock): `ty` PROVES it, EXHAUSTIVELY.
//!   * `Buggy = 1` (the old code, cap reads the all-presents clock): `ty` finds a
//!     COUNTEREXAMPLE — a content-idle-arriving echo deferred past its arrival because a
//!     recent aurora present kept the clock fresh — a genuine, non-vacuous catch.
//!
//! Verification is always required (batteries-on): an absent `ty` FAILS the test with a
//! build hint, so a green run proves `ty` actually model-checked both configs. The
//! certificate re-reds if the modeled coalescing logic drifts.

use aterm_spec::verify::ty_escalation;
use std::path::{Path, PathBuf};
use std::process::Command;

/// `crates/aterm-spec/tests/` → `crates/aterm-spec-models/specs/PresentCoalescing.tla`.
fn spec_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../aterm-spec-models/specs/PresentCoalescing.tla")
}

/// Run `ty check <spec> --config <cfg> [extra...]`; return (exit-0, combined output).
fn run(ty: &PathBuf, spec: &Path, cfg: &Path, extra: &[&str]) -> (bool, String) {
    let out = Command::new(ty)
        .arg("check")
        .arg(spec)
        .arg("--config")
        .arg(cfg)
        .args(extra)
        .output()
        .expect("run ty check");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), combined)
}

#[test]
fn present_coalescing_proves_echo_never_starved_and_catches_aurora_poisoning() {
    // TIERED (VERIFY-1): a hand-written `.tla` — external-tool obligation;
    // runs only where the Trust toolchain is installed (loud notice otherwise).
    let Some(ty) = ty_escalation("PresentCoalescing (latency fix) spec") else {
        return;
    };
    let spec = spec_path();
    assert!(
        spec.exists(),
        "missing committed spec {} — the PCP latency certificate",
        spec.display()
    );
    let okcfg = spec.with_extension("cfg"); // PresentCoalescing.cfg (Buggy = 0)

    // Buggy = 0 (the fix): the efficiency theorem must hold under an EXHAUSTIVE
    // (uncapped) BFS — no echo is ever deferred past one frame interval.
    let (ok, out) = run(&ty, &spec, &okcfg, &["--require-exhaustive"]);
    assert!(
        ok,
        "PresentCoalescing (Buggy=0, the fix) must model-check clean (ContentIdleEchoImmediate)\n{out}"
    );
    assert!(
        out.contains("exhaustive"),
        "the prove run must be an exhaustive (uncapped) BFS, not a bounded sample\n{out}"
    );

    // Buggy = 1 (the OLD code: aurora ticks stamp the cap): MUST yield a
    // counterexample — a reachable state with a starved echo. The cfg flips only the
    // Buggy constant, so the catch is attributable to that single behavior change.
    let dir = std::env::temp_dir().join(format!("aterm-pcp-coalescing-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mk tempdir");
    let bugcfg = dir.join("PresentCoalescing.bug.cfg");
    let oktext = std::fs::read_to_string(&okcfg).expect("read committed cfg");
    let bugtext = oktext.replace("Buggy = 0", "Buggy = 1");
    assert_ne!(oktext, bugtext, "cfg must contain `Buggy = 0` to flip");
    std::fs::write(&bugcfg, bugtext).expect("write bug cfg");
    let (bug_ok, bug_out) = run(&ty, &spec, &bugcfg, &[]);
    assert!(
        !bug_ok,
        "PresentCoalescing (Buggy=1, old code) MUST yield a counterexample (aurora poisoning)\n{bug_out}"
    );
    // The counterexample must be the EFFICIENCY invariant failing — not an unrelated one
    // (e.g. TypeOK) — so a future regression that reds the gate for the wrong reason can't
    // masquerade as a genuine catch.
    assert!(
        bug_out.contains("ContentIdleEchoImmediate"),
        "Buggy=1 must violate ContentIdleEchoImmediate specifically, not another invariant\n{bug_out}"
    );
    let _ = std::fs::remove_dir_all(&dir);

    eprintln!(
        "PCP PresentCoalescing: ContentIdleEchoImmediate proven (Buggy=0, exhaustive) and \
         aurora-poisoning caught (Buggy=1 → counterexample)."
    );
}
